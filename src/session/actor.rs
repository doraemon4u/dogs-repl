//! Session actor: the single writer for [`Store`], plus the bridge between the
//! Transport layer and the Presentation layer.
//!
//! All network events are funnelled into one `Command` channel. The actor is
//! the only task that mutates the store; readers take the `RwLock` read side.
//! REST calls run in short-lived tasks and report back through the same channel
//! as internal events, so a slow request never blocks the socket reader or the
//! renderer.

use crate::config::{self, StoredCredentials};
use crate::engine::board::Color;
use crate::engine::coord::Coord;
use crate::engine::sgf::SgfGame;
use crate::session::store::{
    apply_clock_update, apply_removed_stones, game_channel, game_view_from_json,
    notification_text, parse_ogs_coord, GameView, LogLevel, NotificationItem, Store,
};
use crate::transport::auth::{self, AuthSession};
use crate::transport::realtime_client::{GameList, RealtimeClient, RealtimeEvent};
use crate::transport::rest_client::{ChallengeOptions, RestClient};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

/// Session configuration.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// World channel joined on login and shown on the lobby screen.
    pub world_channel: String,
    /// Where credentials are persisted; `None` disables persistence.
    pub persist_path: Option<PathBuf>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            world_channel: std::env::var("OGS_LANG").unwrap_or_else(|_| "en".to_string()),
            persist_path: Some(config::credentials_path()),
        }
    }
}

/// User- or UI-initiated actions.
pub enum Command {
    Login {
        username: String,
        password: String,
    },
    Resume(StoredCredentials),
    Logout,
    Reconnect,
    /// Switches the chat pane only; world subscription is unaffected.
    SetChatScope(crate::session::store::ChatScope),
    JoinChannel(String),
    PartChannel(String),
    SetWorldChannel(String),
    /// Sends to the current channel (or an explicit one).
    SendChat {
        channel: Option<String>,
        text: String,
    },
    RefreshLobby,
    FetchChannels,
    OpenGame(u64),
    CloseGame(u64),
    LoadSgf(u64),
    LoadMyGames,
    LoadChallenges,
    PlayMove {
        game_id: u64,
        coord: Coord,
    },
    Pass {
        game_id: u64,
    },
    Resign {
        game_id: u64,
    },
    UndoRequest {
        game_id: u64,
        move_number: Option<u64>,
    },
    UndoAccept {
        game_id: u64,
        move_number: u64,
    },
    UndoCancel {
        game_id: u64,
        move_number: u64,
    },
    GameChat {
        game_id: u64,
        text: String,
    },
    EstimateEnter {
        game_id: u64,
    },
    EstimateExit {
        game_id: u64,
    },
    EstimateConfirm {
        game_id: u64,
    },
    ToggleEstimateDead {
        game_id: u64,
        coord: Coord,
    },
    /// Official OGS scoring: mark stones removed or dame.
    OfficialDead {
        game_id: u64,
        coords: Vec<Coord>,
        removed: bool,
    },
    OfficialAccept {
        game_id: u64,
    },
    OfficialReject {
        game_id: u64,
    },
    CreateChallenge(ChallengeOptions),
    AcceptChallenge(i64),
    DeclineChallenge(i64),
    MonitorPlayer(i64),
    LookupPlayer(String),
    Log {
        level: LogLevel,
        text: String,
    },
    Quit,
    Internal(Internal),
}

/// Results of background work, delivered on the same channel as commands.
pub enum Internal {
    LoggedIn(Result<Bootstrap, String>),
    Overview(Result<Value, String>),
    MyGames(Result<Value, String>),
    Challenges(Result<Value, String>),
    Channels(Result<Value, String>),
    UiConfigFallback(Result<Value, String>),
    GameLoaded {
        game_id: u64,
        rest: Result<Value, String>,
        sgf: Result<String, String>,
    },
    SgfLoaded {
        game_id: u64,
        sgf: Result<String, String>,
    },
    ChallengeCreated(Result<Value, String>),
    ChallengeAccepted {
        id: i64,
        result: Result<Value, String>,
    },
    ChallengeDeclined {
        id: i64,
        result: Result<Value, String>,
    },
    PlayerResolved {
        name: String,
        result: Result<Value, String>,
    },
    ReconnectJwt(Result<String, String>),
    Realtime(RealtimeEvent),
}

pub struct Bootstrap {
    auth: AuthSession,
    jwt: String,
    me: Value,
    username: String,
    warning: Option<String>,
}

#[derive(Clone)]
pub struct SessionHandle {
    state: Arc<RwLock<Store>>,
    tx: mpsc::UnboundedSender<Command>,
}

impl SessionHandle {
    /// Starts the actor with a fresh store.
    pub fn spawn(config: SessionConfig) -> Self {
        let store = Store::new(config.world_channel.clone());
        Self::spawn_with_store(config, store)
    }

    /// Starts the actor with a pre-populated store (tests, replay fixtures).
    pub fn spawn_with_store(config: SessionConfig, store: Store) -> Self {
        let state = Arc::new(RwLock::new(store));
        let (tx, rx) = mpsc::unbounded_channel();
        let actor = Actor {
            state: Arc::clone(&state),
            tx: tx.clone(),
            rx,
            config,
            auth: None,
            rest: None,
            realtime: None,
            jwt: None,
            login_in_flight: false,
            flagged: HashSet::new(),
            tick_count: 0,
        };
        tokio::spawn(actor.run());
        Self { state, tx }
    }

    pub fn send(&self, command: Command) {
        if self.tx.send(command).is_err() {
            // The actor is gone; there is nowhere to report this, and the UI
            // will notice that the store stopped changing.
        }
    }

    /// Read access for rendering. Callers must not hold the lock across awaits.
    pub fn state(&self) -> Arc<RwLock<Store>> {
        Arc::clone(&self.state)
    }

    pub async fn snapshot(&self) -> Store {
        self.state.read().await.clone()
    }

    pub fn log(&self, level: LogLevel, text: impl Into<String>) {
        self.send(Command::Log {
            level,
            text: text.into(),
        });
    }

    /// Polls the store until `predicate` is true or the timeout elapses.
    pub async fn wait_for(
        &self,
        timeout: Duration,
        predicate: impl Fn(&Store) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if predicate(&*self.state.read().await) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

struct Actor {
    state: Arc<RwLock<Store>>,
    tx: mpsc::UnboundedSender<Command>,
    rx: mpsc::UnboundedReceiver<Command>,
    config: SessionConfig,
    auth: Option<AuthSession>,
    rest: Option<Arc<RestClient>>,
    realtime: Option<Arc<RealtimeClient>>,
    jwt: Option<String>,
    login_in_flight: bool,
    /// Games whose local clock has already reported a flag to the server.
    flagged: HashSet<u64>,
    tick_count: u64,
}

impl Actor {
    async fn run(mut self) {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                maybe = self.rx.recv() => {
                    let Some(command) = maybe else { break };
                    if !self.handle(command).await {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    self.tick().await;
                }
            }
        }
        self.shutdown().await;
    }

    async fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::Login { username, password } => {
                self.begin_login(BootstrapSource::Password { username, password });
            }
            Command::Resume(stored) => {
                self.begin_login(BootstrapSource::Stored(stored));
            }
            Command::Logout => self.logout().await,
            Command::Reconnect => self.reconnect().await,
            Command::SetChatScope(scope) => {
                let mut state = self.state.write().await;
                state.set_chat_scope(scope);
            }
            Command::JoinChannel(channel) => {
                if let Some(realtime) = &self.realtime {
                    realtime.chat_join(&channel);
                }
                self.state
                    .write()
                    .await
                    .set_channel_joined(&channel, true);
                self.log(LogLevel::Info, format!("joined #{channel}")).await;
            }
            Command::PartChannel(channel) => {
                if let Some(realtime) = &self.realtime {
                    realtime.chat_part(&channel);
                }
                self.state
                    .write()
                    .await
                    .set_channel_joined(&channel, false);
                self.log(LogLevel::Info, format!("left #{channel}")).await;
            }
            Command::SetWorldChannel(channel) => self.set_world_channel(channel).await,
            Command::SendChat { channel, text } => self.send_chat(channel, text).await,
            Command::RefreshLobby => {
                self.spawn_overview();
                self.spawn_my_games();
                self.spawn_challenges();
                self.spawn_channels();
            }
            Command::FetchChannels => self.spawn_channels(),
            Command::OpenGame(game_id) => self.open_game(game_id).await,
            Command::CloseGame(game_id) => self.close_game(game_id).await,
            Command::LoadSgf(game_id) => self.spawn_sgf(game_id),
            Command::LoadMyGames => self.spawn_my_games(),
            Command::LoadChallenges => self.spawn_challenges(),
            Command::PlayMove { game_id, coord } => {
                self.play_move(game_id, coord).await;
            }
            Command::Pass { game_id } => {
                if let Some(realtime) = &self.realtime {
                    realtime.pass(game_id as i64);
                    self.log(LogLevel::Info, format!("game {game_id}: pass sent"))
                        .await;
                } else {
                    self.offline_warning("pass").await;
                }
            }
            Command::Resign { game_id } => {
                if let Some(realtime) = &self.realtime {
                    realtime.resign(game_id as i64);
                    self.log(LogLevel::Warning, format!("game {game_id}: resignation sent"))
                        .await;
                } else {
                    self.offline_warning("resign").await;
                }
            }
            Command::UndoRequest {
                game_id,
                move_number,
            } => {
                let number = match move_number {
                    Some(number) => number,
                    None => self
                        .state
                        .read()
                        .await
                        .game(game_id)
                        .map(|game| u64::from(game.move_number()))
                        .unwrap_or(0),
                };
                if let Some(realtime) = &self.realtime {
                    realtime.request_undo(game_id as i64, number as i64);
                    self.log(
                        LogLevel::Info,
                        format!("game {game_id}: undo requested at move {number}"),
                    )
                    .await;
                }
            }
            Command::UndoAccept {
                game_id,
                move_number,
            } => {
                if let Some(realtime) = &self.realtime {
                    realtime.game_undo_accept(game_id as i64, move_number as i64);
                }
                self.log(
                    LogLevel::Info,
                    format!("game {game_id}: undo accepted"),
                )
                .await;
            }
            Command::UndoCancel {
                game_id,
                move_number,
            } => {
                if let Some(realtime) = &self.realtime {
                    realtime.game_undo_cancel(game_id as i64, move_number as i64);
                }
                self.log(
                    LogLevel::Info,
                    format!("game {game_id}: undo declined"),
                )
                .await;
            }
            Command::GameChat { game_id, text } => self.game_chat(game_id, text).await,
            Command::EstimateEnter { game_id } => {
                let mut state = self.state.write().await;
                if let Some(game) = state.game_mut(game_id) {
                    game.enter_estimate();
                    let summary = game.estimate_summary.clone();
                    drop(state);
                    self.log(
                        LogLevel::Info,
                        format!(
                            "estimate mode: type [a10] to toggle a group, `estimate confirm` to count, `estimate exit` to leave{}",
                            summary
                                .map(|summary| format!(" (current: {summary})"))
                                .unwrap_or_default()
                        ),
                    )
                    .await;
                } else {
                    drop(state);
                    self.log(LogLevel::Warning, format!("game {game_id} is not open"))
                        .await;
                }
            }
            Command::EstimateExit { game_id } => {
                let mut state = self.state.write().await;
                if let Some(game) = state.game_mut(game_id) {
                    game.exit_estimate();
                }
                drop(state);
                self.log(LogLevel::Info, "estimate mode exited").await;
            }
            Command::EstimateConfirm { game_id } => self.estimate_confirm(game_id).await,
            Command::ToggleEstimateDead { game_id, coord } => {
                let mut state = self.state.write().await;
                if let Some(game) = state.game_mut(game_id) {
                    if !game.estimate_active {
                        drop(state);
                        self.log(
                            LogLevel::Warning,
                            "not in estimate mode; run `estimate` first",
                        )
                        .await;
                        return true;
                    }
                    let toggled = game.toggle_estimate_dead(coord);
                    let summary = game.estimate_summary.clone();
                    let dead = game.estimate_dead.len();
                    drop(state);
                    if toggled {
                        self.log(
                            LogLevel::Info,
                            format!(
                                "marked {coord} ({dead} stone(s) marked){}",
                                summary
                                    .map(|summary| format!(" — local: {summary}"))
                                    .unwrap_or_default()
                            ),
                        )
                        .await;
                    } else {
                        self.log(LogLevel::Warning, format!("no stone at {coord}"))
                            .await;
                    }
                } else {
                    drop(state);
                    self.log(LogLevel::Warning, format!("game {game_id} is not open"))
                        .await;
                }
            }
            Command::OfficialDead {
                game_id,
                coords,
                removed,
            } => self.official_dead(game_id, coords, removed).await,
            Command::OfficialAccept { game_id } => self.official_accept(game_id).await,
            Command::OfficialReject { game_id } => {
                if let Some(realtime) = &self.realtime {
                    realtime.game_removed_stones_reject(game_id as i64);
                    self.log(
                        LogLevel::Warning,
                        format!("game {game_id}: official scoring rejected"),
                    )
                    .await;
                }
            }
            Command::CreateChallenge(options) => self.spawn_create_challenge(options),
            Command::AcceptChallenge(id) => self.spawn_accept_challenge(id),
            Command::DeclineChallenge(id) => self.spawn_decline_challenge(id),
            Command::LookupPlayer(name) => self.spawn_lookup_player(name),
            Command::MonitorPlayer(player_id) => {
                if let Some(realtime) = &self.realtime {
                    realtime.monitor_user(&[player_id]);
                }
                self.log(
                    LogLevel::Info,
                    format!("monitoring player {player_id}"),
                )
                .await;
            }
            Command::Log { level, text } => self.log(level, text).await,
            Command::Quit => return false,
            Command::Internal(internal) => self.handle_internal(internal).await,
        }
        true
    }

    // ---------------------------------------------------------------- login

    fn begin_login(&mut self, source: BootstrapSource) {
        if self.login_in_flight {
            return;
        }
        self.login_in_flight = true;
        let tx = self.tx.clone();
        let persist_path = self.config.persist_path.clone();
        tokio::spawn(async move {
            let result = bootstrap(source, persist_path).await;
            let _ = tx.send(Command::Internal(Internal::LoggedIn(result)));
        });
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let mut state = state.write().await;
            state.connection = crate::session::store::ConnectionState::Connecting;
            state.connection_message = Some("signing in…".to_string());
        });
    }

    async fn on_logged_in(&mut self, result: Result<Bootstrap, String>) {
        self.login_in_flight = false;
        match result {
            Ok(bootstrap) => {
                let user_id = bootstrap
                    .me
                    .get("id")
                    .and_then(Value::as_i64);
                {
                    let mut state = self.state.write().await;
                    state.username = Some(bootstrap.username.clone());
                    state.user_id = user_id;
                    state.connection = crate::session::store::ConnectionState::Connecting;
                    state.connection_message = None;
                }
                self.auth = Some(bootstrap.auth.clone());
                self.rest = Some(Arc::new(RestClient::new(bootstrap.auth.clone())));
                if let Some(warning) = &bootstrap.warning {
                    self.log(LogLevel::Warning, warning.clone()).await;
                }
                self.log(
                    LogLevel::Success,
                    format!("signed in as {} (id {})", bootstrap.username, user_id.unwrap_or(0)),
                )
                .await;
                self.start_realtime(bootstrap.jwt).await;
                self.spawn_overview();
                self.spawn_channels();
                self.spawn_my_games();
                self.spawn_challenges();
            }
            Err(error) => {
                {
                    let mut state = self.state.write().await;
                    state.connection = crate::session::store::ConnectionState::Error;
                    state.connection_message = Some(error.clone());
                }
                self.log(LogLevel::Error, format!("login failed: {error}"))
                    .await;
            }
        }
    }

    async fn logout(&mut self) {
        if let Some(realtime) = self.realtime.take() {
            tokio::spawn(async move { realtime.disconnect().await });
        }
        self.auth = None;
        self.rest = None;
        self.jwt = None;
        if let Some(path) = &self.config.persist_path {
            let _ = config::clear_credentials_at(path);
        }
        {
            let mut state = self.state.write().await;
            state.connection = crate::session::store::ConnectionState::Offline;
            state.connection_message = None;
            state.username = None;
            state.user_id = None;
            state.games.clear();
            state.active_game = None;
            state.switch_to_world();
        }
        self.log(LogLevel::Info, "signed out").await;
    }

    async fn reconnect(&mut self) {
        let Some(rest) = self.rest.clone() else {
            self.log(LogLevel::Warning, "not signed in yet").await;
            return;
        };
        self.log(LogLevel::Info, "reconnecting…").await;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = rest
                .ui_config()
                .await
                .map_err(|error| format!("GET /ui/config failed: {error}"))
                .and_then(|config| {
                    config
                        .get("user_jwt")
                        .and_then(Value::as_str)
                        .filter(|jwt| !jwt.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| "no user_jwt in /ui/config".to_string())
                });
            let _ = tx.send(Command::Internal(Internal::ReconnectJwt(result)));
        });
    }

    async fn start_realtime(&mut self, jwt: String) {
        if let Some(old) = self.realtime.take() {
            tokio::spawn(async move { old.disconnect().await });
        }
        let mut client = RealtimeClient::new(jwt.clone());
        if let Some(mut events) = client.take_event_receiver() {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    if tx.send(Command::Internal(Internal::Realtime(event))).is_err() {
                        break;
                    }
                }
            });
        }
        let client = Arc::new(client);
        self.jwt = Some(jwt);
        self.realtime = Some(Arc::clone(&client));
        tokio::spawn(async move { client.connect().await });
    }

    // --------------------------------------------------------------- games

    async fn open_game(&mut self, game_id: u64) {
        let mut state = self.state.write().await;
        state.switch_to_game(game_id);
        let size = state
            .games
            .get(&game_id)
            .map(|game| game.size)
            .unwrap_or(19);
        state
            .games
            .entry(game_id)
            .or_insert_with(|| GameView::empty(game_id, size));
        drop(state);
        if let Some(realtime) = &self.realtime {
            realtime.connect_to_game(game_id as i64);
        }
        self.log(LogLevel::Info, format!("opening game {game_id}…"))
            .await;
        self.spawn_game_load(game_id);
    }

    async fn close_game(&mut self, game_id: u64) {
        if let Some(realtime) = &self.realtime {
            realtime.disconnect_from_game(game_id as i64);
        }
        {
            let mut state = self.state.write().await;
            if state.active_game == Some(game_id) {
                state.active_game = None;
            }
            if let Some(game) = state.game_mut(game_id) {
                game.connected = false;
            }
            state.switch_to_world();
        }
        self.log(LogLevel::Info, format!("closed game {game_id}"))
            .await;
    }

    async fn play_move(&mut self, game_id: u64, coord: Coord) {
        let (size, phase, to_play) = {
            let state = self.state.read().await;
            match state.game(game_id) {
                Some(game) => (game.size, game.phase, Some(game.to_play())),
                None => (19, crate::session::store::GamePhase::Play, None),
            }
        };
        if !coord.is_on_board(size) {
            self.log(
                LogLevel::Error,
                format!("{coord} is outside a {size}x{size} board"),
            )
            .await;
            return;
        }
        if phase != crate::session::store::GamePhase::Play {
            self.log(
                LogLevel::Warning,
                format!("game {game_id} is in {} phase; moves are closed", phase.label()),
            )
            .await;
            return;
        }
        if let Some(realtime) = &self.realtime {
            realtime.play_move(game_id as i64, &coord.to_gtp());
            self.log(
                LogLevel::Info,
                format!(
                    "game {game_id}: play {} ({} to move) sent",
                    coord.to_gtp(),
                    to_play.map(|color| format!("{color:?}")).unwrap_or_default()
                ),
            )
            .await;
        } else {
            self.offline_warning("play").await;
        }
    }

    async fn game_chat(&mut self, game_id: u64, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let move_number = self
            .state
            .read()
            .await
            .game(game_id)
            .map(|game| i64::from(game.move_number()))
            .unwrap_or(0);
        if let Some(realtime) = &self.realtime {
            realtime.game_chat(game_id as i64, move_number, &text);
            self.state
                .write()
                .await
                .push_system_chat(&game_channel(game_id), format!("you: {text}"));
        } else {
            self.offline_warning("game chat").await;
        }
    }

    async fn send_chat(&mut self, channel: Option<String>, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let channel = channel.unwrap_or_else(|| {
            // Read the current scope without awaiting inside the closure.
            self.state
                .try_read()
                .map(|state| state.current_chat_channel())
                .unwrap_or_else(|_| self.config.world_channel.clone())
        });
        if let Some(realtime) = &self.realtime {
            if let Some(game_id) = channel
                .strip_prefix("game/")
                .and_then(|id| id.parse::<u64>().ok())
            {
                let move_number = self
                    .state
                    .read()
                    .await
                    .game(game_id)
                    .map(|game| i64::from(game.move_number()))
                    .unwrap_or(0);
                realtime.game_chat(game_id as i64, move_number, &text);
            } else {
                realtime.send_chat(&channel, &text);
            }
            self.state
                .write()
                .await
                .push_system_chat(&channel, format!("you: {text}"));
        } else {
            self.offline_warning("chat").await;
        }
    }

    async fn set_world_channel(&mut self, channel: String) {
        let (old, was_joined) = {
            let state = self.state.read().await;
            let old = state.world_channel.clone();
            let joined = state
                .channels
                .get(&old)
                .map(|channel| channel.joined)
                .unwrap_or(false);
            (old, joined)
        };
        if old == channel {
            self.state
                .write()
                .await
                .set_chat_scope(crate::session::store::ChatScope::world(&channel));
            return;
        }
        if let Some(realtime) = &self.realtime {
            if was_joined {
                realtime.chat_part(&old);
            }
            realtime.chat_join(&channel);
        }
        {
            let mut state = self.state.write().await;
            state.set_world_channel(&channel);
            state.set_channel_joined(&channel, true);
            state.set_channel_joined(&old, false);
        }
        self.log(
            LogLevel::Info,
            format!("world channel switched to #{channel}"),
        )
        .await;
    }

    // ----------------------------------------------------------- estimate

    async fn estimate_confirm(&mut self, game_id: u64) {
        let summary = {
            let mut state = self.state.write().await;
            let Some(game) = state.game_mut(game_id) else {
                drop(state);
                self.log(LogLevel::Warning, format!("game {game_id} is not open"))
                    .await;
                return;
            };
            if !game.estimate_active {
                game.enter_estimate();
            }
            game.recompute_estimate()
        };
        self.log(
            LogLevel::Success,
            format!(
                "local estimate: {summary} — this is not the official OGS result. \
                 Official scoring is negotiated with `dead <coord>`, `score accept`, `score reject`."
            ),
        )
        .await;
    }

    async fn official_dead(&mut self, game_id: u64, coords: Vec<Coord>, removed: bool) {
        if coords.is_empty() {
            self.log(LogLevel::Warning, "no coordinates supplied").await;
            return;
        }
        let stones: Vec<String> = coords.iter().map(|coord| coord.to_gtp()).collect();
        if let Some(realtime) = &self.realtime {
            realtime.game_removed_stones_set(game_id as i64, removed, &stones);
        }
        {
            let mut state = self.state.write().await;
            if let Some(game) = state.game_mut(game_id) {
                for coord in &coords {
                    let point = (coord.col, coord.row);
                    if removed {
                        game.scoring_dead.insert(point);
                    } else {
                        game.scoring_dead.remove(&point);
                    }
                }
                game.scoring_last_message = Some(format!(
                    "{}: {}",
                    if removed { "marked dead" } else { "marked dame" },
                    stones.join(" ")
                ));
            }
        }
        self.log(
            LogLevel::Info,
            format!(
                "game {game_id}: official scoring {} {}",
                if removed { "dead" } else { "dame" },
                stones.join(" ")
            ),
        )
        .await;
    }

    async fn official_accept(&mut self, game_id: u64) {
        let (stones, count) = {
            let state = self.state.read().await;
            let mut points: Vec<_> = state
                .game(game_id)
                .map(|game| game.scoring_dead.iter().copied().collect())
                .unwrap_or_default();
            points.sort_unstable();
            let stones = points
                .iter()
                .filter_map(|&(col, row)| {
                    let coord = Coord::new(col, row);
                    coord
                        .is_on_board(state.game(game_id).map(|game| game.size).unwrap_or(19))
                        .then(|| coord.to_gtp())
                })
                .collect::<Vec<_>>()
                .join(",");
            (stones, points.len())
        };
        if let Some(realtime) = &self.realtime {
            realtime.game_removed_stones_accept(game_id as i64, &stones);
        }
        self.log(
            LogLevel::Success,
            format!("game {game_id}: accepted official scoring ({count} dead stone(s))"),
        )
        .await;
    }

    // ---------------------------------------------------------------- tick

    async fn tick(&mut self) {
        let flagged: Vec<(u64, Color)> = {
            let mut state = self.state.write().await;
            state.tick_clocks(Duration::from_secs(1));
            state
                .games
                .values()
                .filter(|game| game.clock.flagged().is_some() && !self.flagged.contains(&game.id))
                .filter_map(|game| game.clock.flagged().map(|color| (game.id, color)))
                .collect()
        };
        for (game_id, color) in flagged {
            self.flagged.insert(game_id);
            if let Some(realtime) = &self.realtime {
                realtime.game_timed_out(game_id as i64);
            }
            self.log(
                LogLevel::Warning,
                format!("game {game_id}: {color:?} lost on time"),
            )
            .await;
        }
        self.tick_count = self.tick_count.wrapping_add(1);
        if self.tick_count % 60 == 0
            && self.state.read().await.connection
                == crate::session::store::ConnectionState::Authenticated
        {
            self.spawn_overview();
        }
    }

    // ------------------------------------------------------------ realtime

    async fn handle_realtime(&mut self, event: RealtimeEvent) {
        match event {
            RealtimeEvent::Connected => {
                {
                    let mut state = self.state.write().await;
                    state.connection = crate::session::store::ConnectionState::Connected;
                    state.connection_message = None;
                }
                self.log(LogLevel::Info, "realtime socket connected").await;
                self.join_world_channel().await;
            }
            RealtimeEvent::Disconnected => {
                {
                    let mut state = self.state.write().await;
                    state.connection = crate::session::store::ConnectionState::Connecting;
                    state.connection_message = Some("socket disconnected; reconnecting".into());
                }
                self.log(LogLevel::Warning, "realtime socket disconnected; reconnecting").await;
            }
            RealtimeEvent::Authenticated { user_id, username } => {
                {
                    let mut state = self.state.write().await;
                    state.connection = crate::session::store::ConnectionState::Authenticated;
                    state.connection_message = None;
                    if user_id != 0 {
                        state.user_id = Some(user_id);
                    }
                    if !username.is_empty() {
                        state.username = Some(username.clone());
                    }
                }
                self.log(
                    LogLevel::Success,
                    format!("socket authenticated as {username} ({user_id})"),
                )
                .await;
                self.join_world_channel().await;
                if let Some(realtime) = &self.realtime {
                    realtime.gamelist_count_subscribe(Some("live"));
                    realtime.stats_online(60);
                    realtime.query_game_list(GameList::Live, json!({}), 0, 30);
                    let active = self.state.read().await.active_game;
                    if let Some(game_id) = active {
                        realtime.connect_to_game(game_id as i64);
                    }
                }
                self.spawn_overview();
            }
            RealtimeEvent::Heartbeat => {}
            RealtimeEvent::HostInfo(info) => {
                let version = info
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                self.log(LogLevel::Info, format!("OGS server version {version}"))
                    .await;
            }
            RealtimeEvent::Notification(note) => {
                let text = notification_text(&note);
                let id = note
                    .get("id")
                    .or_else(|| note.get("notification_id"))
                    .map(|value| match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                self.state.write().await.push_notification(NotificationItem {
                    id,
                    text: text.clone(),
                    at: None,
                });
                self.log(LogLevel::Info, format!("notification: {text}"))
                    .await;
            }
            RealtimeEvent::ActiveGame(game) => {
                let id = game
                    .get("game_id")
                    .or_else(|| game.get("id"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if id != 0 {
                    self.log(LogLevel::Info, format!("active game detected: {id}"))
                        .await;
                }
            }
            RealtimeEvent::GameListCount { list, count } => {
                self.state
                    .write()
                    .await
                    .lobby_counts
                    .insert(list, count);
            }
            RealtimeEvent::Chat {
                channel,
                author,
                text,
            } => {
                self.state.write().await.push_chat(&channel, &author, &text);
            }
            RealtimeEvent::GameEvent { event, data } => {
                self.handle_game_event(&event, data).await;
            }
            RealtimeEvent::Response {
                command,
                data,
                error,
                ..
            } => {
                self.handle_response(&command, data, error).await;
            }
            RealtimeEvent::Error(message) => {
                self.state
                    .write()
                    .await
                    .connection_message = Some(message.clone());
                self.log(LogLevel::Error, format!("socket: {message}"))
                    .await;
            }
        }
    }

    async fn join_world_channel(&mut self) {
        let channel = self.state.read().await.world_channel.clone();
        if let Some(realtime) = &self.realtime {
            realtime.chat_join(&channel);
        }
        self.state
            .write()
            .await
            .set_channel_joined(&channel, true);
    }

    async fn handle_game_event(&mut self, event: &str, data: Value) {
        let Some(game_id) = game_id_from_event(event) else {
            return;
        };
        let suffix = event.rsplit('/').next().unwrap_or_default();
        match suffix {
            "gamedata" => {
                let user_id = self.state.read().await.user_id;
                let payload = if data.get("game_id").is_none() && data.get("id").is_none() {
                    let mut object = data.clone();
                    if let Some(map) = object.as_object_mut() {
                        map.insert("game_id".to_string(), json!(game_id));
                    }
                    object
                } else {
                    data
                };
                if let Some(mut view) = game_view_from_json(&payload, user_id) {
                    view.id = game_id;
                    let title = view.title();
                    let mut state = self.state.write().await;
                    if let Some(old) = state.games.get(&game_id) {
                        view.estimate_active = old.estimate_active;
                        view.estimate_dead = old.estimate_dead.clone();
                        view.estimate_summary = old.estimate_summary.clone();
                        if old.our_color.is_some() {
                            view.our_color = old.our_color;
                        }
                    }
                    state.games.insert(game_id, view);
                    drop(state);
                    self.log(LogLevel::Info, format!("game {game_id}: {title}"))
                        .await;
                }
            }
            "move" => self.apply_move_event(game_id, data).await,
            "clock" => {
                let mut state = self.state.write().await;
                if let Some(game) = state.game_mut(game_id) {
                    apply_clock_update(game, &data);
                }
            }
            "phase" => {
                let phase = data
                    .get("phase")
                    .and_then(Value::as_str)
                    .or_else(|| data.as_str())
                    .unwrap_or_default();
                let phase = crate::session::store::GamePhase::from_ogs(phase);
                {
                    let mut state = self.state.write().await;
                    if let Some(game) = state.game_mut(game_id) {
                        game.phase = phase;
                        game.clock.set_active(match phase {
                            crate::session::store::GamePhase::Play => Some(game.to_play()),
                            _ => None,
                        });
                    }
                }
                self.log(
                    LogLevel::Info,
                    format!("game {game_id}: phase {}", phase.label()),
                )
                .await;
            }
            "removed_stones" => {
                let mut state = self.state.write().await;
                if let Some(game) = state.game_mut(game_id) {
                    apply_removed_stones(game, &data);
                    game.phase = crate::session::store::GamePhase::StoneRemoval;
                }
                drop(state);
                self.log(
                    LogLevel::Info,
                    format!("game {game_id}: opponent updated the official dead stones"),
                )
                .await;
            }
            "undo_requested" => {
                let move_number = data
                    .get("move_number")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                self.log(
                    LogLevel::Warning,
                    format!("game {game_id}: opponent requests undo at move {move_number} (use `undo accept` or `undo decline`)"),
                )
                .await;
            }
            "undo_accepted" => {
                self.log(LogLevel::Info, format!("game {game_id}: undo accepted"))
                    .await;
                self.spawn_game_load(game_id);
            }
            "undo_canceled" | "undo_rejected" => {
                self.log(LogLevel::Info, format!("game {game_id}: undo declined"))
                    .await;
            }
            "chat" => {
                let author = data
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = data
                    .get("text")
                    .or_else(|| data.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.state
                    .write()
                    .await
                    .push_chat(&game_channel(game_id), author, text);
            }
            "resign" | "ended" | "game_ends" => {
                let result = data
                    .get("result")
                    .or_else(|| data.get("winner"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                {
                    let mut state = self.state.write().await;
                    if let Some(game) = state.game_mut(game_id) {
                        game.phase = crate::session::store::GamePhase::Finished;
                        game.result = result.clone();
                        game.clock.set_active(None);
                    }
                }
                self.log(
                    LogLevel::Success,
                    format!(
                        "game {game_id}: finished{}",
                        result
                            .map(|result| format!(" ({result})"))
                            .unwrap_or_default()
                    ),
                )
                .await;
            }
            "error" => {
                self.log(
                    LogLevel::Error,
                    format!("game {game_id}: {data}"),
                )
                .await;
            }
            _ => {}
        }
    }

    async fn apply_move_event(&mut self, game_id: u64, data: Value) {
        let raw = data.get("move").unwrap_or(&data);
        let is_pass = move_is_pass(raw);
        let event_number = data.get("move_number").and_then(Value::as_u64);
        let (size, current, phase) = {
            let state = self.state.read().await;
            match state.game(game_id) {
                Some(game) => (game.size, game.move_number(), game.phase),
                None => {
                    drop(state);
                    self.spawn_game_load(game_id);
                    return;
                }
            }
        };
        let expected = u64::from(current) + 1;
        if let Some(number) = event_number {
            if number <= u64::from(current) {
                return;
            }
            if number > expected {
                self.log(
                    LogLevel::Warning,
                    format!("game {game_id}: missed moves (at {current}, event {number}); resyncing"),
                )
                .await;
                self.spawn_game_load(game_id);
                return;
            }
        }
        let coord = if is_pass {
            None
        } else {
            match parse_ogs_coord(raw, size) {
                Some(coord) => Some(coord),
                None => {
                    self.log(
                        LogLevel::Warning,
                        format!("game {game_id}: could not parse move {raw}"),
                    )
                    .await;
                    return;
                }
            }
        };
        if phase != crate::session::store::GamePhase::Play {
            self.log(
                LogLevel::Warning,
                format!("game {game_id}: unexpected move in {} phase", phase.label()),
            )
            .await;
        }
        let result = {
            let mut state = self.state.write().await;
            state
                .game_mut(game_id)
                .ok_or_else(|| "game disappeared".to_string())
                .and_then(|game| game.apply_server_move(coord))
        };
        if let Err(error) = result {
            self.log(LogLevel::Error, format!("game {game_id}: {error}"))
                .await;
            self.spawn_game_load(game_id);
        }
    }

    async fn handle_response(&mut self, command: &str, data: Value, error: Option<Value>) {
        if error.is_some() {
            // RealtimeClient already surfaced this as RealtimeEvent::Error.
            return;
        }
        match command {
            "gamelist/query" => {
                self.state.write().await.apply_game_list(&data);
            }
            "stats/online" => {
                let online = data
                    .get("online")
                    .or_else(|| data.get("count"))
                    .and_then(Value::as_u64)
                    .or_else(|| data.as_u64());
                if let Some(online) = online {
                    self.state.write().await.online = Some(online);
                }
            }
            "hostinfo" => {
                let version = data
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                self.log(LogLevel::Info, format!("OGS server version {version}"))
                    .await;
            }
            "game/connect" => {
                self.log(LogLevel::Info, format!("game stream connected: {data}"))
                    .await;
            }
            "game/removed_stones/set" | "game/removed_stones/accept" => {
                self.log(
                    LogLevel::Info,
                    format!("official scoring update accepted: {data}"),
                )
                .await;
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- internal

    async fn handle_internal(&mut self, internal: Internal) {
        match internal {
            Internal::LoggedIn(result) => self.on_logged_in(result).await,
            Internal::Realtime(event) => self.handle_realtime(event).await,
            Internal::Overview(result) => match result {
                Ok(value) => self.state.write().await.apply_overview(&value),
                Err(error) => {
                    self.log(LogLevel::Warning, format!("lobby refresh failed: {error}"))
                        .await
                }
            },
            Internal::MyGames(result) => match result {
                Ok(value) => {
                    self.state.write().await.apply_my_games(&value);
                    let count = self.state.read().await.my_games.len();
                    self.log(LogLevel::Info, format!("{count} active game(s)"))
                        .await;
                }
                Err(error) => {
                    self.log(LogLevel::Warning, format!("GET /me/games failed: {error}"))
                        .await
                }
            },
            Internal::Challenges(result) => match result {
                Ok(value) => {
                    self.state.write().await.apply_my_challenges(&value);
                    let count = self.state.read().await.my_challenges.len();
                    self.log(
                        LogLevel::Info,
                        format!("{count} challenge(s) addressed to you"),
                    )
                    .await;
                }
                Err(error) => {
                    self.log(
                        LogLevel::Warning,
                        format!("GET /me/challenges failed: {error}"),
                    )
                    .await
                }
            },
            Internal::Channels(result) => match result {
                Ok(value) => {
                    let mut state = self.state.write().await;
                    state.apply_chat_channels(&value);
                    let empty = state.available_channels.is_empty();
                    drop(state);
                    if empty {
                        self.spawn_channel_fallback();
                    }
                }
                Err(error) => {
                    self.log(
                        LogLevel::Warning,
                        format!("chat channel list failed ({error}); falling back to /ui/config"),
                    )
                    .await;
                    self.spawn_channel_fallback();
                }
            },
            Internal::UiConfigFallback(result) => match result {
                Ok(value) => {
                    let channels = value
                        .get("language_channels")
                        .or_else(|| value.get("chat_channels"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let mut state = self.state.write().await;
                    state.apply_chat_channels(&channels);
                    let names = state.available_channels.clone();
                    drop(state);
                    if names.is_empty() {
                        self.log(
                            LogLevel::Warning,
                            "no language channels advertised; using the configured default",
                        )
                        .await;
                    } else {
                        self.log(
                            LogLevel::Info,
                            format!("channels: {}", names.join(", ")),
                        )
                        .await;
                    }
                }
                Err(error) => {
                    self.log(
                        LogLevel::Warning,
                        format!("could not load language channels: {error}"),
                    )
                    .await
                }
            },
            Internal::GameLoaded {
                game_id,
                rest,
                sgf,
            } => self.apply_game_load(game_id, rest, sgf).await,
            Internal::SgfLoaded { game_id, sgf } => {
                self.apply_game_load(game_id, Ok(Value::Null), sgf).await
            }
            Internal::ChallengeCreated(result) => match result {
                Ok(value) => {
                    let id = value.get("id").and_then(Value::as_i64).unwrap_or(0);
                    self.log(
                        LogLevel::Success,
                        format!("challenge created (id {id})"),
                    )
                    .await;
                    self.spawn_challenges();
                }
                Err(error) => {
                    self.log(LogLevel::Error, format!("challenge failed: {error}"))
                        .await
                }
            },
            Internal::ChallengeAccepted { id, result } => match result {
                Ok(_) => {
                    self.log(
                        LogLevel::Success,
                        format!("challenge {id} accepted"),
                    )
                    .await;
                    self.spawn_my_games();
                }
                Err(error) => {
                    self.log(
                        LogLevel::Error,
                        format!("accept challenge {id} failed: {error}"),
                    )
                    .await
                }
            },
            Internal::ChallengeDeclined { id, result } => match result {
                Ok(_) => {
                    self.log(LogLevel::Info, format!("challenge {id} declined"))
                        .await
                }
                Err(error) => {
                    self.log(
                        LogLevel::Error,
                        format!("decline challenge {id} failed: {error}"),
                    )
                    .await
                }
            },
            Internal::PlayerResolved { name, result } => match result {
                Ok(player) => {
                    let id = player.get("id").and_then(Value::as_i64).unwrap_or(0);
                    let rating = player
                        .get("ratings")
                        .and_then(|ratings| ratings.get("overall"))
                        .and_then(|overall| overall.get("rating"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let username = player
                        .get("username")
                        .and_then(Value::as_str)
                        .unwrap_or(&name);
                    self.log(
                        LogLevel::Info,
                        format!("{username} (id {id}): rating {rating:.1}"),
                    )
                    .await;
                }
                Err(error) => {
                    self.log(
                        LogLevel::Error,
                        format!("player lookup {name} failed: {error}"),
                    )
                    .await
                }
            },
            Internal::ReconnectJwt(result) => match result {
                Ok(jwt) => self.start_realtime(jwt).await,
                Err(error) => {
                    self.log(LogLevel::Error, format!("reconnect failed: {error}"))
                        .await
                }
            },
        }
    }

    async fn apply_game_load(
        &mut self,
        game_id: u64,
        rest: Result<Value, String>,
        sgf: Result<String, String>,
    ) {
        let user_id = self.state.read().await.user_id;
        let from_rest = rest
            .as_ref()
            .ok()
            .and_then(|value| game_view_from_json(value, user_id));
        let sgf_game = sgf
            .as_ref()
            .ok()
            .and_then(|text| SgfGame::parse(text).ok());
        let mut warning = None;
        if let Err(error) = &rest {
            warning = Some(format!("GET /games/{game_id} failed: {error}"));
        }
        if let Err(error) = &sgf {
            if warning.is_none() {
                warning = Some(format!("GET /games/{game_id}/sgf failed: {error}"));
            }
        }

        let mut view = from_rest.unwrap_or_else(|| GameView::empty(game_id, 19));
        if let Some(meta) = sgf_game {
            match meta.replay_game() {
                Ok(game) => {
                    view.id = game_id;
                    view.size = game.size();
                    view.game = game;
                    view.clock.set_active(Some(view.game.to_play()));
                    if view.black == "Black" {
                        if let Some(name) = &meta.player_black {
                            view.black = name.clone();
                        }
                    }
                    if view.white == "White" {
                        if let Some(name) = &meta.player_white {
                            view.white = name.clone();
                        }
                    }
                    if !meta.setup_black.is_empty() || !meta.setup_white.is_empty() {
                        view.scoring_last_message =
                            Some("setup stones loaded from SGF".to_string());
                    }
                }
                Err(error) => {
                    warning = Some(format!("SGF replay failed: {error}"));
                }
            }
        }
        view.connected = true;
        {
            let mut state = self.state.write().await;
            state.games.insert(game_id, view);
        }
        if let Some(warning) = warning {
            self.log(LogLevel::Warning, warning).await;
        }
        self.log(LogLevel::Success, format!("game {game_id} loaded"))
            .await;
    }

    // --------------------------------------------------------------- spawns

    fn spawn_internal<F>(&self, future: F)
    where
        F: Future<Output = Internal> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Command::Internal(future.await));
        });
    }

    fn spawn_overview(&self) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move { Internal::Overview(rest.overview().await) });
    }

    fn spawn_my_games(&self) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move { Internal::MyGames(rest.my_active_games().await) });
    }

    fn spawn_challenges(&self) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move { Internal::Challenges(rest.my_challenges().await) });
    }

    fn spawn_channels(&self) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move { Internal::Channels(rest.chat_channels().await) });
    }

    fn spawn_channel_fallback(&self) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move {
            Internal::UiConfigFallback(rest.ui_config().await)
        });
    }

    fn spawn_game_load(&self, game_id: u64) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move {
            let rest_result = rest.game(&game_id.to_string()).await;
            let sgf_result = rest.game_sgf(&game_id.to_string()).await;
            Internal::GameLoaded {
                game_id,
                rest: rest_result,
                sgf: sgf_result,
            }
        });
    }

    fn spawn_sgf(&self, game_id: u64) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move {
            Internal::SgfLoaded {
                game_id,
                sgf: rest.game_sgf(&game_id.to_string()).await,
            }
        });
    }

    fn spawn_create_challenge(&self, options: ChallengeOptions) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move {
            let mut options = options;
            if options.opponent_id.is_none() {
                match rest.player_by_username(&options.opponent).await {
                    Ok(player) => {
                        options.opponent_id = player.get("id").and_then(Value::as_i64);
                    }
                    Err(error) => {
                        return Internal::ChallengeCreated(Err(format!(
                            "could not resolve opponent {}: {error}",
                            options.opponent
                        )));
                    }
                }
            }
            Internal::ChallengeCreated(rest.create_challenge(&options).await)
        });
    }

    fn spawn_accept_challenge(&self, id: i64) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move {
            Internal::ChallengeAccepted {
                id,
                result: rest.accept_challenge(&id.to_string()).await,
            }
        });
    }

    fn spawn_lookup_player(&self, name: String) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move {
            let result = match name.parse::<i64>() {
                Ok(id) => rest.player(id).await,
                Err(_) => rest.player_by_username(&name).await,
            };
            Internal::PlayerResolved { name, result }
        });
    }

    fn spawn_decline_challenge(&self, id: i64) {
        let Some(rest) = self.rest.clone() else { return };
        self.spawn_internal(async move {
            Internal::ChallengeDeclined {
                id,
                result: rest.decline_challenge(&id.to_string()).await,
            }
        });
    }

    async fn offline_warning(&self, action: &str) {
        self.log(
            LogLevel::Warning,
            format!("cannot {action}: realtime socket is not connected (run `login`/`connect`)"),
        )
        .await;
    }

    async fn log(&self, level: LogLevel, text: impl Into<String>) {
        self.state.write().await.push_log(level, text);
    }

    async fn shutdown(&mut self) {
        if let Some(realtime) = self.realtime.take() {
            realtime.disconnect().await;
        }
        if let Some(auth) = &self.auth {
            let _ = auth.persist().await;
        }
    }
}

enum BootstrapSource {
    Password { username: String, password: String },
    Stored(StoredCredentials),
}

async fn bootstrap(
    source: BootstrapSource,
    persist_path: Option<PathBuf>,
) -> Result<Bootstrap, String> {
    let (auth_session, hint) = match source {
        BootstrapSource::Password { username, password } => {
            let tokens = auth::login(&username, &password).await?;
            let session = AuthSession::new(tokens, persist_path);
            session.persist().await?;
            (session, username)
        }
        BootstrapSource::Stored(stored) => {
            let hint = stored.username.clone();
            let session = AuthSession::from_stored(stored, persist_path);
            session
                .ensure_valid()
                .await
                .map_err(|error| format!("stored token refresh failed: {error}"))?;
            (session, hint)
        }
    };

    let rest = RestClient::new(auth_session.clone());
    let me = rest
        .me()
        .await
        .map_err(|error| format!("GET /me failed: {error}"))?;
    let user_id = me.get("id").and_then(Value::as_i64);
    if let Some(user_id) = user_id {
        auth_session.set_user_id(user_id).await;
    }
    let username = me
        .get("username")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or(hint);
    let config = rest
        .ui_config()
        .await
        .map_err(|error| format!("GET /ui/config failed: {error}"))?;
    let jwt = config
        .get("user_jwt")
        .and_then(Value::as_str)
        .filter(|jwt| !jwt.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            "GET /ui/config returned no user_jwt; the access token may not be accepted"
                .to_string()
        })?;
    let mut warning = None;
    if auth::jwt_is_anonymous(&jwt) == Some(true) {
        return Err(
            "server returned an anonymous user_jwt, so socket authentication would be a guest"
                .to_string(),
        );
    }
    if auth::jwt_claims(&jwt).is_none() {
        warning = Some(
            "user_jwt was not a decodable JWT; socket authentication could not be pre-verified"
                .to_string(),
        );
    }
    let _ = auth_session.persist().await;

    Ok(Bootstrap {
        auth: auth_session,
        jwt,
        me,
        username,
        warning,
    })
}

fn game_id_from_event(event: &str) -> Option<u64> {
    event
        .strip_prefix("game/")?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// True when a wire move value represents a pass.
fn move_is_pass(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items
            .iter()
            .take(2)
            .any(|item| item.as_i64().map(|number| number < 0).unwrap_or(false)),
        Value::String(text) => {
            let text = text.trim();
            text.is_empty() || text == ".." || text.eq_ignore_ascii_case("pass")
        }
        Value::Object(map) => map
            .get("x")
            .and_then(Value::as_i64)
            .map(|x| x < 0)
            .unwrap_or(false)
            || map
                .get("y")
                .and_then(Value::as_i64)
                .map(|y| y < 0)
                .unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::coord::Coord;
    use crate::session::store::GamePhase;
    use crate::session::store::LogLevel;
    use std::time::Duration;

    fn config() -> SessionConfig {
        SessionConfig {
            world_channel: "en".to_string(),
            persist_path: None,
        }
    }

    fn store_with_game() -> Store {
        let mut store = Store::new("en");
        let mut game = GameView::empty(42, 9);
        game.black = "alice".into();
        game.white = "bob".into();
        game.our_color = Some(Color::Black);
        game.game.play(Coord::new(2, 2)).unwrap();
        game.game.play(Coord::new(6, 6)).unwrap();
        store.games.insert(42, game);
        store.switch_to_game(42);
        store
    }

    #[test]
    fn parses_game_ids_from_event_names() {
        assert_eq!(game_id_from_event("game/1234/gamedata"), Some(1234));
        assert_eq!(game_id_from_event("game/9/move"), Some(9));
        assert_eq!(game_id_from_event("notification"), None);
        assert_eq!(game_id_from_event("game/abc/move"), None);
    }

    #[test]
    fn pass_detection_covers_wire_shapes() {
        assert!(move_is_pass(&json!(null)));
        assert!(move_is_pass(&json!([-1, -1])));
        assert!(move_is_pass(&json!("..")));
        assert!(move_is_pass(&json!("pass")));
        assert!(!move_is_pass(&json!([3, 4])));
        assert!(!move_is_pass(&json!("D4")));
    }

    #[tokio::test]
    async fn estimate_commands_mutate_the_store_through_the_actor() {
        let handle = SessionHandle::spawn_with_store(config(), store_with_game());
        handle.send(Command::EstimateEnter { game_id: 42 });
        assert!(
            handle
                .wait_for(Duration::from_secs(1), |store| store
                    .game(42)
                    .map(|game| game.estimate_active)
                    .unwrap_or(false))
                .await
        );
        handle.send(Command::ToggleEstimateDead {
            game_id: 42,
            coord: Coord::new(2, 2),
        });
        assert!(
            handle
                .wait_for(Duration::from_secs(1), |store| store
                    .game(42)
                    .map(|game| game.estimate_dead.contains(&(2, 2)))
                    .unwrap_or(false))
                .await
        );
        handle.send(Command::EstimateConfirm { game_id: 42 });
        assert!(
            handle
                .wait_for(Duration::from_secs(1), |store| store
                    .game(42)
                    .and_then(|game| game.estimate_summary.clone())
                    .is_some())
                .await
        );
        handle.send(Command::EstimateExit { game_id: 42 });
        assert!(
            handle
                .wait_for(Duration::from_secs(1), |store| store
                    .game(42)
                    .map(|game| !game.estimate_active)
                    .unwrap_or(false))
                .await
        );
    }

    #[tokio::test]
    async fn chat_scope_switch_keeps_world_unread_in_the_actor() {
        let handle = SessionHandle::spawn_with_store(config(), Store::new("en"));
        handle.send(Command::OpenGame(7));
        assert!(
            handle
                .wait_for(Duration::from_secs(1), |store| {
                    store.chat_scope.game_id() == Some(7)
                })
                .await
        );
        // No REST client, so the game load cannot run; the scope still moved
        // and the world channel remains subscribed.
        handle.send(Command::SetChatScope(
            crate::session::store::ChatScope::Game { game_id: 7 },
        ));
        let _ = handle.snapshot().await;
    }

    #[tokio::test]
    async fn engine_clock_tick_is_driven_by_the_actor() {
        let mut store = Store::new("en");
        let mut game = GameView::empty(3, 9);
        game.clock = crate::engine::clock::Clock::new(
            crate::engine::clock::TimeControl::Absolute {
                main: Duration::from_secs(3),
            },
            Some(Color::Black),
        );
        store.games.insert(3, game);
        let handle = SessionHandle::spawn_with_store(config(), store);
        // Actor ticks once per second; give it enough wall-clock time for 2-3.
        tokio::time::sleep(Duration::from_millis(2200)).await;
        let state = handle.snapshot().await;
        let remaining = state.game(3).unwrap().clock.player(Color::Black).main;
        assert!(
            remaining < Duration::from_secs(3),
            "clock did not tick: {remaining:?}"
        );
    }

    #[tokio::test]
    async fn log_command_appends_to_the_store() {
        let handle = SessionHandle::spawn_with_store(config(), Store::new("en"));
        handle.send(Command::Log {
            level: LogLevel::Success,
            text: "hello".into(),
        });
        assert!(
            handle
                .wait_for(Duration::from_secs(1), |store| store
                    .logs
                    .iter()
                    .any(|line| line.text == "hello"))
                .await
        );
    }

    #[test]
    fn phase_from_ogs_is_case_insensitive() {
        assert_eq!(GamePhase::from_ogs("Stone Removal"), GamePhase::StoneRemoval);
        assert_eq!(GamePhase::from_ogs("FINISHED"), GamePhase::Finished);
        assert_eq!(GamePhase::from_ogs("play"), GamePhase::Play);
    }
}
