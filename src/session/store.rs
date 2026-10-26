//! Global session state.
//!
//! Concurrency model (audit item 4): a single actor task owns every mutation.
//! The renderer and the text REPL only ever read a [`Store`] behind an
//! `Arc<RwLock<_>>`; they never write. The actor takes the write lock for short,
//! synchronous state updates and never holds it across an `await`, so a slow
//! network call cannot stall rendering.
//!
//! Chat scope (design §8.2): switching to a game changes only `chat_scope`; the
//! world channel subscription is kept and its unread counter keeps growing in
//! the background.

use crate::engine::board::{Board, Color};
use crate::engine::clock::{Clock, TimeControl};
use crate::engine::coord::Coord;
use crate::engine::estimate::{
    connected_stone_group, estimate_with_dead, score, PointEstimate, ScoreSummary, ScoringRules,
};
use crate::engine::game::Game;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::{Duration, SystemTime};

/// Maximum number of chat lines retained per channel.
pub const CHAT_CAPACITY: usize = 500;
/// Maximum number of log lines retained.
pub const LOG_CAPACITY: usize = 500;

/// A board point in Engine coordinates.
pub type Point = (u8, u16);

/// Realtime connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Offline,
    Connecting,
    Connected,
    Authenticated,
    Error,
}

impl ConnectionState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Authenticated => "authenticated",
            Self::Error => "error",
        }
    }
}

/// Where the chat pane is looking. The world subscription is independent of
/// this value and stays active while a game is in focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatScope {
    World { channel: String },
    Game { game_id: u64 },
}

impl ChatScope {
    pub fn world(channel: impl Into<String>) -> Self {
        Self::World {
            channel: channel.into(),
        }
    }

    /// The storage key of the channel this scope shows.
    pub fn channel_key(&self) -> String {
        match self {
            Self::World { channel } => channel.clone(),
            Self::Game { game_id } => game_channel(*game_id),
        }
    }

    pub fn game_id(&self) -> Option<u64> {
        match self {
            Self::Game { game_id } => Some(*game_id),
            Self::World { .. } => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::World { channel } => format!("#{channel}"),
            Self::Game { game_id } => format!("game {game_id}"),
        }
    }
}

/// Storage key for a game's chat channel.
pub fn game_channel(game_id: u64) -> String {
    format!("game/{game_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    Chat,
    System,
}

#[derive(Debug, Clone)]
pub struct ChatLine {
    /// Monotonic sequence across all channels, for incremental readers.
    pub seq: u64,
    pub channel: String,
    pub author: String,
    pub text: String,
    pub kind: ChatKind,
    pub at: SystemTime,
}

#[derive(Debug, Clone, Default)]
pub struct ChannelState {
    pub messages: VecDeque<ChatLine>,
    pub unread: u32,
    pub joined: bool,
    pub topic: Option<String>,
}

impl ChannelState {
    fn push(&mut self, line: ChatLine) {
        self.messages.push_back(line);
        while self.messages.len() > CHAT_CAPACITY {
            self.messages.pop_front();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub seq: u64,
    pub level: LogLevel,
    pub text: String,
    pub at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GamePhase {
    #[default]
    Play,
    StoneRemoval,
    Finished,
    Unknown,
}

impl GamePhase {
    pub fn from_ogs(phase: &str) -> Self {
        match phase.to_ascii_lowercase().as_str() {
            "play" | "" => Self::Play,
            "stone removal" | "stone_removal" | "scoring" => Self::StoneRemoval,
            "finished" | "ended" => Self::Finished,
            _ => Self::Unknown,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::StoneRemoval => "scoring",
            Self::Finished => "finished",
            Self::Unknown => "unknown",
        }
    }
}

/// A challenge row.
#[derive(Debug, Clone, Default)]
pub struct ChallengeSummary {
    pub id: i64,
    pub challenger: String,
    pub board_size: u8,
    pub rules: String,
    pub ranked: bool,
}

/// A lobby game row.
#[derive(Debug, Clone, Default)]
pub struct LobbyGame {
    pub id: i64,
    pub black: String,
    pub white: String,
    pub board_size: u8,
    pub phase: String,
    pub move_number: i64,
    pub rules: String,
}

impl LobbyGame {
    pub fn label(&self) -> String {
        format!(
            "#{} {} vs {} ({}x{}) {}",
            self.id, self.black, self.white, self.board_size, self.board_size, self.phase
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct NotificationItem {
    pub id: String,
    pub text: String,
    pub at: Option<SystemTime>,
}

/// A locally maintained mirror of one game.
#[derive(Debug, Clone)]
pub struct GameView {
    pub id: u64,
    pub black: String,
    pub white: String,
    pub black_id: Option<i64>,
    pub white_id: Option<i64>,
    pub size: u8,
    pub komi: f64,
    pub rules: String,
    pub phase: GamePhase,
    pub game: Game,
    pub clock: Clock,
    pub our_color: Option<Color>,
    pub connected: bool,
    pub result: Option<String>,
    /// Local estimate mode (design §10): user-marked dead stones.
    pub estimate_active: bool,
    pub estimate_dead: HashSet<Point>,
    pub estimate_summary: Option<ScoreSummary>,
    /// Official server-side stone-removal negotiation state.
    pub scoring_dead: HashSet<Point>,
    pub scoring_last_message: Option<String>,
}

impl GameView {
    pub fn empty(id: u64, size: u8) -> Self {
        Self {
            id,
            black: "Black".to_string(),
            white: "White".to_string(),
            black_id: None,
            white_id: None,
            size,
            komi: 6.5,
            rules: "chinese".to_string(),
            phase: GamePhase::Play,
            game: Game::new(size),
            clock: Clock::new(TimeControl::None, Some(game_start_color())),
            our_color: None,
            connected: false,
            result: None,
            estimate_active: false,
            estimate_dead: HashSet::new(),
            estimate_summary: None,
            scoring_dead: HashSet::new(),
            scoring_last_message: None,
        }
    }

    pub fn board(&self) -> &Board {
        self.game.board()
    }

    pub fn to_play(&self) -> Color {
        self.game.to_play()
    }

    pub fn move_number(&self) -> u16 {
        self.game.move_number()
    }

    pub fn last_move(&self) -> Option<Coord> {
        self.game.last_move().and_then(|mv| mv.coord)
    }

    /// The overlay used by estimate mode.
    pub fn estimate_overlay(&self) -> Vec<Vec<PointEstimate>> {
        estimate_with_dead(self.board(), &self.estimate_dead)
    }

    /// Recomputes and stores the local estimate. Local only: this is never sent
    /// as the official result.
    pub fn recompute_estimate(&mut self) -> ScoreSummary {
        let summary = score(
            self.board(),
            &self.estimate_dead,
            self.komi,
            ScoringRules::from_ogs(&self.rules),
            (
                self.game.captures(Color::Black),
                self.game.captures(Color::White),
            ),
        );
        self.estimate_summary = Some(summary.clone());
        summary
    }

    /// Toggles the dead marker of the connected group at `coord`.
    pub fn toggle_estimate_dead(&mut self, coord: Coord) -> bool {
        if !coord.is_on_board(self.size) {
            return false;
        }
        let group = connected_stone_group(self.board(), (coord.col, coord.row));
        if group.is_empty() {
            return false;
        }
        let all_dead = group
            .iter()
            .all(|point| self.estimate_dead.contains(point));
        if all_dead {
            for point in group {
                self.estimate_dead.remove(&point);
            }
        } else {
            for point in group {
                self.estimate_dead.insert(point);
            }
        }
        self.recompute_estimate();
        true
    }

    pub fn enter_estimate(&mut self) {
        self.estimate_active = true;
        self.recompute_estimate();
    }

    pub fn exit_estimate(&mut self) {
        self.estimate_active = false;
        self.estimate_dead.clear();
        self.estimate_summary = None;
    }

    /// Applies a move that arrived from the server.
    pub fn apply_server_move(&mut self, coord: Option<Coord>) -> Result<(), String> {
        let color = self.game.to_play();
        match coord {
            Some(coord) => {
                self.game
                    .play(coord)
                    .map_err(|error| format!("server move {} rejected: {error}", coord))?;
            }
            None => {
                self.game.pass();
            }
        }
        self.clock.on_move(color);
        if self.estimate_active {
            self.recompute_estimate();
        }
        Ok(())
    }

    /// A compact game line for the TUI title.
    pub fn title(&self) -> String {
        format!(
            "{} vs {} · {}x{} · move {} · {}",
            self.black,
            self.white,
            self.size,
            self.size,
            self.game.move_number(),
            self.phase.label()
        )
    }
}

fn game_start_color() -> Color {
    Color::Black
}

/// Cross-screen session state.
#[derive(Debug, Clone)]
pub struct Store {
    pub connection: ConnectionState,
    pub connection_message: Option<String>,
    pub username: Option<String>,
    pub user_id: Option<i64>,
    pub games: BTreeMap<u64, GameView>,
    pub active_game: Option<u64>,
    pub chat_scope: ChatScope,
    pub channels: BTreeMap<String, ChannelState>,
    pub available_channels: Vec<String>,
    pub notifications: VecDeque<NotificationItem>,
    pub open_challenges: Vec<ChallengeSummary>,
    pub my_challenges: Vec<ChallengeSummary>,
    pub lobby_games: Vec<LobbyGame>,
    pub my_games: Vec<LobbyGame>,
    pub lobby_counts: BTreeMap<String, u64>,
    pub online: Option<u64>,
    pub logs: VecDeque<LogLine>,
    pub log_seq: u64,
    pub chat_seq: u64,
    pub world_channel: String,
}

impl Default for Store {
    fn default() -> Self {
        Self::new("en")
    }
}

impl Store {
    pub fn new(world_channel: impl Into<String>) -> Self {
        let world_channel = world_channel.into();
        let mut channels = BTreeMap::new();
        channels.insert(world_channel.clone(), ChannelState::default());
        Self {
            connection: ConnectionState::Offline,
            connection_message: None,
            username: None,
            user_id: None,
            games: BTreeMap::new(),
            active_game: None,
            chat_scope: ChatScope::World {
                channel: world_channel.clone(),
            },
            channels,
            available_channels: Vec::new(),
            notifications: VecDeque::new(),
            open_challenges: Vec::new(),
            my_challenges: Vec::new(),
            lobby_games: Vec::new(),
            my_games: Vec::new(),
            lobby_counts: BTreeMap::new(),
            online: None,
            logs: VecDeque::new(),
            log_seq: 0,
            chat_seq: 0,
            world_channel,
        }
    }

    /// Appends a log line and returns its sequence number.
    pub fn push_log(&mut self, level: LogLevel, text: impl Into<String>) -> u64 {
        self.log_seq += 1;
        self.logs.push_back(LogLine {
            seq: self.log_seq,
            level,
            text: text.into(),
            at: SystemTime::now(),
        });
        while self.logs.len() > LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.log_seq
    }

    pub fn ensure_channel(&mut self, channel: &str) -> &mut ChannelState {
        self.channels.entry(channel.to_string()).or_default()
    }

    /// The channel key the chat pane is currently showing.
    pub fn current_chat_channel(&self) -> String {
        self.chat_scope.channel_key()
    }

    /// Adds an incoming chat line and updates unread counters.
    pub fn push_chat(&mut self, channel: &str, author: &str, text: &str) {
        let current = self.current_chat_channel();
        self.chat_seq += 1;
        let line = ChatLine {
            seq: self.chat_seq,
            channel: channel.to_string(),
            author: author.to_string(),
            text: text.to_string(),
            kind: ChatKind::Chat,
            at: SystemTime::now(),
        };
        let update_unread = current != channel;
        let state = self.ensure_channel(channel);
        state.push(line);
        if update_unread {
            state.unread = state.unread.saturating_add(1);
        }
    }

    /// Adds a local system line to a channel.
    pub fn push_system_chat(&mut self, channel: &str, text: impl Into<String>) {
        self.chat_seq += 1;
        let line = ChatLine {
            seq: self.chat_seq,
            channel: channel.to_string(),
            author: String::new(),
            text: text.into(),
            kind: ChatKind::System,
            at: SystemTime::now(),
        };
        self.ensure_channel(channel).push(line);
    }

    /// Switches the chat pane without unsubscribing the world channel.
    pub fn set_chat_scope(&mut self, scope: ChatScope) {
        let key = scope.channel_key();
        self.chat_scope = scope;
        if let Some(channel) = self.channels.get_mut(&key) {
            channel.unread = 0;
        }
    }

    pub fn switch_to_world(&mut self) {
        let channel = self.world_channel.clone();
        self.set_chat_scope(ChatScope::World { channel });
        self.active_game = None;
    }

    pub fn switch_to_game(&mut self, game_id: u64) {
        self.active_game = Some(game_id);
        self.ensure_channel(&game_channel(game_id));
        self.set_chat_scope(ChatScope::Game { game_id });
    }

    pub fn unread_total(&self) -> u32 {
        self.channels.values().map(|channel| channel.unread).sum()
    }

    /// Marks a channel joined/left without discarding its history.
    pub fn set_channel_joined(&mut self, channel: &str, joined: bool) {
        self.ensure_channel(channel).joined = joined;
    }

    /// Sets the active world channel, keeping the previous channel's history.
    pub fn set_world_channel(&mut self, channel: &str) {
        self.world_channel = channel.to_string();
        self.ensure_channel(channel);
        if matches!(self.chat_scope, ChatScope::World { .. }) {
            self.set_chat_scope(ChatScope::world(channel));
        }
    }

    pub fn game(&self, game_id: u64) -> Option<&GameView> {
        self.games.get(&game_id)
    }

    pub fn game_mut(&mut self, game_id: u64) -> Option<&mut GameView> {
        self.games.get_mut(&game_id)
    }

    /// The game the chat pane and board should show.
    pub fn focused_game(&self) -> Option<&GameView> {
        self.active_game
            .and_then(|id| self.games.get(&id))
            .or_else(|| self.chat_scope.game_id().and_then(|id| self.games.get(&id)))
    }

    /// Advances every running clock. Called once per second by the actor.
    pub fn tick_clocks(&mut self, elapsed: Duration) {
        for game in self.games.values_mut() {
            if game.phase == GamePhase::Play {
                game.clock.tick(elapsed);
            }
        }
    }

    /// Adds one pending notification, replacing an entry with the same id.
    pub fn push_notification(&mut self, item: NotificationItem) {
        if !item.id.is_empty() {
            if let Some(existing) = self
                .notifications
                .iter_mut()
                .find(|existing| existing.id == item.id)
            {
                *existing = item;
                return;
            }
        }
        self.notifications.push_back(item);
        while self.notifications.len() > 100 {
            self.notifications.pop_front();
        }
    }

    /// Applies `/ui/overview`.
    pub fn apply_overview(&mut self, value: &Value) {
        if let Some(games) = value.get("active_games").and_then(Value::as_array) {
            self.lobby_games = games.iter().filter_map(lobby_game_from).collect();
        }
        if let Some(challenges) = value.get("challenges").and_then(Value::as_array) {
            self.open_challenges = challenges
                .iter()
                .filter_map(|challenge| challenge_from(challenge, false))
                .collect();
        }
        self.online = value
            .get("online")
            .or_else(|| value.get("users_online"))
            .or_else(|| value.get("online_count"))
            .and_then(Value::as_u64)
            .or(self.online);
    }

    /// Applies a `gamelist/query` array.
    pub fn apply_game_list(&mut self, value: &Value) {
        let Some(games) = value.as_array() else { return };
        self.lobby_games = games.iter().filter_map(lobby_game_from).collect();
    }

    /// Applies `/me/games`.
    pub fn apply_my_games(&mut self, value: &Value) {
        let rows = value
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| value.as_array().cloned())
            .unwrap_or_default();
        self.my_games = rows.iter().filter_map(lobby_game_from).collect();
    }

    /// Applies `/me/challenges`.
    pub fn apply_my_challenges(&mut self, value: &Value) {
        let rows = value
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| value.as_array().cloned())
            .unwrap_or_default();
        self.my_challenges = rows
            .iter()
            .filter_map(|challenge| challenge_from(challenge, true))
            .collect();
    }

    /// Applies `/chat/channels/` (or the equivalent part of `/ui/config`).
    pub fn apply_chat_channels(&mut self, value: &Value) {
        let rows = value
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| value.as_array().cloned())
            .unwrap_or_default();
        let mut names = Vec::new();
        for row in rows {
            let name = row
                .get("name")
                .or_else(|| row.get("id"))
                .and_then(value_as_string)
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let topic = row
                .get("topic")
                .and_then(Value::as_str)
                .map(str::to_string);
            self.ensure_channel(&name).topic = topic;
            names.push(name);
        }
        if !names.is_empty() {
            self.available_channels = names;
        }
    }

    /// Latest notification count.
    pub fn notification_count(&self) -> usize {
        self.notifications.len()
    }
}

/// Extracts a string from a JSON string or number.
fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn player_name(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "?".to_string();
    };
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    value
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

fn player_id(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(|player| player.get("id").or(Some(player)))
        .and_then(Value::as_i64)
}

fn lobby_game_from(value: &Value) -> Option<LobbyGame> {
    let id = value.get("id").and_then(Value::as_i64)?;
    let players = value.get("players");
    let size = value
        .get("width")
        .or_else(|| value.get("board_size"))
        .or_else(|| value.get("height"))
        .and_then(Value::as_u64)
        .unwrap_or(19) as u8;
    let black = players
        .and_then(|players| players.get("black"))
        .map(|player| player_name(Some(player)))
        .unwrap_or_else(|| "?".to_string());
    let white = players
        .and_then(|players| players.get("white"))
        .map(|player| player_name(Some(player)))
        .unwrap_or_else(|| "?".to_string());
    Some(LobbyGame {
        id,
        black,
        white,
        board_size: size,
        phase: value
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("play")
            .to_string(),
        move_number: value
            .get("move_number")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        rules: value
            .get("rules")
            .and_then(Value::as_str)
            .unwrap_or("chinese")
            .to_string(),
    })
}

fn challenge_from(value: &Value, mine: bool) -> Option<ChallengeSummary> {
    let id = value
        .get("id")
        .or_else(|| value.get("challenge_id"))
        .and_then(Value::as_i64)?;
    let challenger = if mine {
        value
            .get("challenger")
            .or_else(|| value.get("challenged"))
            .or_else(|| value.get("player"))
    } else {
        value
            .get("challenger")
            .or_else(|| value.get("player"))
            .or_else(|| value.get("challenged"))
    };
    Some(ChallengeSummary {
        id,
        challenger: player_name(challenger),
        board_size: value
            .get("board_size")
            .or_else(|| value.get("width"))
            .and_then(Value::as_u64)
            .unwrap_or(19) as u8,
        rules: value
            .get("rules")
            .and_then(Value::as_str)
            .unwrap_or("chinese")
            .to_string(),
        ranked: value
            .get("ranked")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

/// Builds a [`TimeControl`] from an OGS game JSON object.
pub fn time_control_from_json(value: &Value) -> TimeControl {
    let number = |key: &str| value.get(key).and_then(Value::as_f64);
    let main = number("main_time").unwrap_or(0.0);
    let increment = number("time_increment").unwrap_or(0.0);
    let period = number("period_time").unwrap_or(0.0);
    let periods = value
        .get("periods")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let system = value
        .get("time_control")
        .or_else(|| value.get("system"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    if main <= 0.0 && increment <= 0.0 && period <= 0.0 {
        return TimeControl::None;
    }
    if system.contains("canadian") {
        return TimeControl::Canadian {
            main: Duration::from_secs_f64(main.max(0.0)),
            stones: value
                .get("stones_per_period")
                .and_then(Value::as_u64)
                .unwrap_or(25) as u32,
            period: Duration::from_secs_f64(period.max(0.0)),
        };
    }
    if system.contains("fischer")
        || (increment > 0.0 && period <= 0.0 && periods == 0)
    {
        return TimeControl::Fischer {
            main: Duration::from_secs_f64(main.max(0.0)),
            increment: Duration::from_secs_f64(increment.max(0.0)),
        };
    }
    if system.contains("byoyomi") || period > 0.0 || periods > 0 {
        return TimeControl::Byoyomi {
            main: Duration::from_secs_f64(main.max(0.0)),
            periods: periods.max(1),
            period: Duration::from_secs_f64(period.max(0.0)),
        };
    }
    TimeControl::Absolute {
        main: Duration::from_secs_f64(main.max(0.0)),
    }
}

/// Parses one coordinate in any of the shapes OGS uses.
///
/// * `[x, y]` with `y` counted from the top (the gamedata/move event form)
/// * `[-1, -1]` or `null` for a pass
/// * `"D4"` (GTP) or `"dd"` (SGF)
pub fn parse_ogs_coord(value: &Value, board_size: u8) -> Option<Coord> {
    match value {
        Value::Null => None,
        Value::Array(items) => {
            let x = items.first().and_then(Value::as_i64)?;
            let y = items.get(1).and_then(Value::as_i64)?;
            if x < 0 || y < 0 {
                return None;
            }
            if x >= board_size as i64 || y >= board_size as i64 {
                return None;
            }
            Some(Coord::new(x as u8, u16::from(board_size - y as u8)))
        }
        Value::Object(map) => {
            let x = map.get("x").and_then(Value::as_i64)?;
            let y = map.get("y").and_then(Value::as_i64)?;
            if x < 0 || y < 0 || x >= board_size as i64 || y >= board_size as i64 {
                return None;
            }
            Some(Coord::new(x as u8, u16::from(board_size - y as u8)))
        }
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() || text == ".." || text.eq_ignore_ascii_case("pass") {
                return None;
            }
            if let Ok(coord) = Coord::from_gtp(text) {
                return coord.is_on_board(board_size).then_some(coord);
            }
            // SGF: two lowercase letters, row from the top.
            let bytes = text.as_bytes();
            if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() {
                let col = bytes[0].to_ascii_lowercase().wrapping_sub(b'a');
                let row_top = bytes[1].to_ascii_lowercase().wrapping_sub(b'a');
                if col < board_size && row_top < board_size {
                    return Some(Coord::new(col, u16::from(board_size - row_top)));
                }
            }
            None
        }
        _ => None,
    }
}

/// Extracts a coordinate list from an array, string, or comma-separated list of OGS coordinates.
pub fn parse_ogs_coords(value: &Value, board_size: u8) -> Vec<Coord> {
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .filter_map(|item| parse_ogs_coord(item, board_size))
            .collect();
    }
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        // Comma or whitespace-separated list: "D4, E5" or "D4 E5"
        if text.contains(',') || text.contains(' ') {
            return text
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter_map(|s| parse_ogs_coord(&Value::String(s.to_string()), board_size))
                .collect();
        }
        // Single coordinate: "D4" or "aa"
        if let Some(c) = parse_ogs_coord(value, board_size) {
            return vec![c];
        }
        // SGF compact sequence of 2-character coordinates: "aabbcc..."
        if text.len() >= 2 && text.len() % 2 == 0 {
            let bytes = text.as_bytes();
            let mut coords = Vec::with_capacity(text.len() / 2);
            for chunk in bytes.chunks_exact(2) {
                if let Ok(s) = std::str::from_utf8(chunk) {
                    if let Some(c) = parse_ogs_coord(&Value::String(s.to_string()), board_size) {
                        coords.push(c);
                    }
                }
            }
            if !coords.is_empty() {
                return coords;
            }
        }
    }
    Vec::new()
}

/// Parses an `initial_state` object (`{black: "aabb...", white: "..."}`) into
/// setup stones.
pub fn parse_initial_state(value: &Value, board_size: u8) -> Vec<(Color, Coord)> {
    let mut stones = Vec::new();
    for (key, color) in [("black", Color::Black), ("white", Color::White)] {
        let Some(text) = value.get(key).and_then(Value::as_str) else {
            continue;
        };
        let bytes = text.as_bytes();
        for pair in bytes.chunks_exact(2) {
            let col = pair[0].wrapping_sub(b'a');
            let row_top = pair[1].wrapping_sub(b'a');
            if col < board_size && row_top < board_size {
                stones.push((color, Coord::new(col, u16::from(board_size - row_top))));
            }
        }
    }
    stones
}

/// Parses the `moves` array of a gamedata payload into colors and coordinates.
pub fn parse_gamedata_moves(
    value: &Value,
    board_size: u8,
    starts_with_white: bool,
) -> Vec<(Color, Option<Coord>)> {
    let Some(moves) = value.get("moves").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut color = if starts_with_white {
        Color::White
    } else {
        Color::Black
    };
    let mut out = Vec::with_capacity(moves.len());
    for mv in moves {
        // Each move may be a bare coordinate or an object with a `move` field.
        let raw = mv.get("move").unwrap_or(mv);
        // A `color` field, when present, wins over alternation.
        if let Some(explicit) = mv.get("color").and_then(Value::as_u64) {
            color = if explicit == 2 {
                Color::White
            } else {
                Color::Black
            };
        }
        out.push((color, parse_ogs_coord(raw, board_size)));
        color = color.opponent();
    }
    out
}

/// Builds a [`GameView`] from a gamedata-ish JSON object plus a user id.
pub fn game_view_from_json(value: &Value, user_id: Option<i64>) -> Option<GameView> {
    let id = value
        .get("game_id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_i64)? as u64;
    let size = value
        .get("width")
        .or_else(|| value.get("board_size"))
        .or_else(|| value.get("height"))
        .and_then(Value::as_u64)
        .unwrap_or(19) as u8;
    let players = value.get("players");
    let black_id = players
        .and_then(|players| player_id(players.get("black")))
        .or_else(|| value.get("black_player_id").and_then(Value::as_i64));
    let white_id = players
        .and_then(|players| player_id(players.get("white")))
        .or_else(|| value.get("white_player_id").and_then(Value::as_i64));
    let black = players
        .map(|players| player_name(players.get("black")))
        .unwrap_or_else(|| "Black".to_string());
    let white = players
        .map(|players| player_name(players.get("white")))
        .unwrap_or_else(|| "White".to_string());

    let initial = value
        .get("initial_state")
        .map(|state| parse_initial_state(state, size))
        .unwrap_or_default();
    let handicap = value.get("handicap").and_then(Value::as_u64).unwrap_or(0);
    // Handicap games start with White; the initial-state stones are Black's.
    let starts_with_white = handicap > 0
        || initial.iter().any(|(color, _)| *color == Color::Black);
    let moves = parse_gamedata_moves(value, size, starts_with_white);
    let game = Game::from_setup_and_moves(size, &initial, &moves)
        .unwrap_or_else(|_| Game::new(size));

    let komi = value.get("komi").and_then(Value::as_f64).unwrap_or(6.5);
    let rules = value
        .get("rules")
        .and_then(Value::as_str)
        .unwrap_or("chinese")
        .to_string();
    let phase = GamePhase::from_ogs(
        value
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("play"),
    );
    let mut clock = Clock::new(
        time_control_from_json(
            value.get("time_control").unwrap_or(&Value::Null),
        ),
        Some(game.to_play()),
    );
    if let Some(clock_json) = value.get("clock").and_then(Value::as_object) {
        if let Some(black) = clock_json.get("black_time").and_then(clock_seconds) {
            clock.set_main(Color::Black, black);
        }
        if let Some(white) = clock_json.get("white_time").and_then(clock_seconds) {
            clock.set_main(Color::White, white);
        }
        if let Some(current) = clock_json.get("current_player").and_then(Value::as_u64) {
            clock.set_active(Some(if current == 2 {
                Color::White
            } else {
                Color::Black
            }));
        }
    }
    if let Some(player_to_move) = value.get("player_to_move").and_then(Value::as_u64) {
        clock.set_active(Some(if player_to_move == 2 {
            Color::White
        } else {
            Color::Black
        }));
    }

    let our_color = match (user_id, black_id, white_id) {
        (Some(uid), Some(black), Some(white)) if black == white => {
            let _ = uid;
            None
        }
        (Some(uid), Some(black), _) if uid == black => Some(Color::Black),
        (Some(uid), _, Some(white)) if uid == white => Some(Color::White),
        _ => None,
    };

    let mut view = GameView {
        id,
        black,
        white,
        black_id,
        white_id,
        size,
        komi,
        rules,
        phase,
        game,
        clock,
        our_color,
        connected: true,
        result: value
            .get("result")
            .or_else(|| value.get("winner"))
            .and_then(Value::as_str)
            .map(str::to_string),
        estimate_active: false,
        estimate_dead: HashSet::new(),
        estimate_summary: None,
        scoring_dead: HashSet::new(),
        scoring_last_message: None,
    };
    if let Some(removed) = value
        .get("removed_stones")
        .or_else(|| value.get("removed"))
    {
        for coord in parse_ogs_coords(removed, size) {
            view.scoring_dead.insert((coord.col, coord.row));
        }
    }
    Some(view)
}

/// Reads a remaining-time value, accepting a number or an object.
fn clock_seconds(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::Object(map) => ["remaining", "main", "main_time", "time"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_f64)),
        _ => None,
    }
}

/// Applies a `game/<id>/clock` push.
pub fn apply_clock_update(game: &mut GameView, value: &Value) {
    for (key, color) in [("black_time", Color::Black), ("white_time", Color::White)] {
        if let Some(seconds) = value.get(key).and_then(clock_seconds) {
            game.clock.set_main(color, seconds);
        }
    }
    if let Some(current) = value
        .get("current_player")
        .or_else(|| value.get("player_to_move"))
        .and_then(Value::as_u64)
    {
        game.clock.set_active(Some(if current == 2 {
            Color::White
        } else {
            Color::Black
        }));
    }
}

/// Applies a `removed_stones` payload (official scoring proposal).
pub fn apply_removed_stones(game: &mut GameView, value: &Value) {
    let list = value
        .get("removed_stones")
        .or_else(|| value.get("stones"))
        .unwrap_or(value);
    game.scoring_dead.clear();
    for coord in parse_ogs_coords(list, game.size) {
        game.scoring_dead.insert((coord.col, coord.row));
    }
}

/// A human-readable one-line summary of a notification payload.
pub fn notification_text(value: &Value) -> String {
    if let Some(text) = value
        .get("message")
        .or_else(|| value.get("text"))
        .and_then(Value::as_str)
    {
        return text.to_string();
    }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("notification");
    format!("{kind}: {value}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::coord::parse_coord;
    use serde_json::json;

    fn game_json() -> Value {
        json!({
            "game_id": 1234,
            "width": 9,
            "height": 9,
            "komi": 5.5,
            "rules": "japanese",
            "phase": "play",
            "handicap": 0,
            "players": {
                "black": {"id": 11, "username": "alice"},
                "white": {"id": 22, "username": "bob"}
            },
            "moves": [[2, 2], [6, 6], [-1, -1]],
            "player_to_move": 2,
            "time_control": {"time_control": "byoyomi", "main_time": 600, "period_time": 30, "periods": 5},
            "clock": {"black_time": 590, "white_time": 580, "current_player": 2}
        })
    }

    #[test]
    fn chat_scope_keeps_world_history_and_unread() {
        let mut store = Store::new("en");
        store.set_channel_joined("en", true);
        store.push_chat("en", "alice", "hello");
        assert_eq!(store.ensure_channel("en").unread, 0);

        store.switch_to_game(7);
        store.push_chat("en", "bob", "while away");
        assert_eq!(store.ensure_channel("en").unread, 1);
        assert_eq!(store.current_chat_channel(), "game/7");

        store.push_chat("game/7", "carol", "hi game");
        assert_eq!(store.ensure_channel("game/7").unread, 0);
        assert_eq!(store.ensure_channel("en").messages.len(), 2);

        // Switching back clears the world unread count but keeps the lines.
        store.switch_to_world();
        assert_eq!(store.current_chat_channel(), "en");
        assert_eq!(store.ensure_channel("en").unread, 0);
        assert_eq!(store.ensure_channel("en").messages.len(), 2);
        assert_eq!(store.unread_total(), 0);
    }

    #[test]
    fn chat_lines_are_capped() {
        let mut store = Store::new("en");
        for index in 0..(CHAT_CAPACITY + 20) {
            store.push_chat("en", "a", &index.to_string());
        }
        assert_eq!(store.ensure_channel("en").messages.len(), CHAT_CAPACITY);
    }

    #[test]
    fn switching_world_channel_keeps_old_history() {
        let mut store = Store::new("en");
        store.push_chat("en", "a", "old");
        store.set_world_channel("zh");
        assert_eq!(store.chat_scope.channel_key(), "zh");
        assert_eq!(store.ensure_channel("en").messages.len(), 1);
        store.set_world_channel("en");
        assert_eq!(store.current_chat_channel(), "en");
    }

    #[test]
    fn logs_have_monotonic_sequence_numbers() {
        let mut store = Store::new("en");
        let first = store.push_log(LogLevel::Info, "one");
        let second = store.push_log(LogLevel::Error, "two");
        assert_eq!(first + 1, second);
        assert_eq!(store.logs.back().unwrap().seq, second);
    }

    #[test]
    fn parses_gamedata_into_a_game_view() {
        let view = game_view_from_json(&game_json(), Some(22)).unwrap();
        assert_eq!(view.id, 1234);
        assert_eq!(view.size, 9);
        assert_eq!(view.black, "alice");
        assert_eq!(view.white, "bob");
        assert_eq!(view.our_color, Some(Color::White));
        assert_eq!(view.komi, 5.5);
        assert_eq!(view.rules, "japanese");
        assert_eq!(view.move_number(), 3);
        assert_eq!(view.board().get(2, 7), Some(Color::Black));
        assert_eq!(view.board().get(6, 3), Some(Color::White));
        assert_eq!(view.clock.active(), Some(Color::White));
        assert_eq!(
            view.clock.player(Color::Black).main,
            Duration::from_secs(590)
        );
    }

    #[test]
    fn recognises_our_color_by_id() {
        let view = game_view_from_json(&game_json(), Some(11)).unwrap();
        assert_eq!(view.our_color, Some(Color::Black));
        let unknown = game_view_from_json(&game_json(), Some(99)).unwrap();
        assert_eq!(unknown.our_color, None);
    }

    #[test]
    fn applies_a_server_move_and_switches_turn() {
        let mut view = game_view_from_json(&game_json(), Some(11)).unwrap();
        // White is to move (player_to_move 2), and the mirror's to_play is
        // White after two moves.
        assert_eq!(view.to_play(), Color::White);
        view.apply_server_move(Some(Coord::new(4, 4))).unwrap();
        assert_eq!(view.board().get(4, 4), Some(Color::White));
        assert_eq!(view.move_number(), 4);
        assert_eq!(view.to_play(), Color::Black);
        view.apply_server_move(None).unwrap();
        assert_eq!(view.move_number(), 5);
    }

    #[test]
    fn parses_coordinates_in_several_wire_shapes() {
        assert_eq!(parse_ogs_coord(&json!([2, 2]), 9), Some(Coord::new(2, 7)));
        assert_eq!(parse_ogs_coord(&json!([-1, -1]), 9), None);
        assert_eq!(parse_ogs_coord(&json!(null), 9), None);
        assert_eq!(parse_ogs_coord(&json!("D4"), 19), parse_coord("[D4]").ok());
        assert_eq!(parse_ogs_coord(&json!("dd"), 19), Some(Coord::new(3, 16)));
        assert_eq!(parse_ogs_coord(&json!({"x": 3, "y": 4}), 19), Some(Coord::new(3, 15)));
        assert_eq!(parse_ogs_coord(&json!([20, 2]), 9), None);
    }

    #[test]
    fn parses_handicap_initial_state() {
        let stones = parse_initial_state(&json!({"black": "ccdd", "white": ""}), 9);
        assert_eq!(stones.len(), 2);
        assert_eq!(stones[0], (Color::Black, Coord::new(2, 7)));
        assert_eq!(stones[1], (Color::Black, Coord::new(3, 6)));
    }

    #[test]
    fn gamedata_handicap_starts_with_white() {
        let value = json!({
            "game_id": 1,
            "width": 9,
            "handicap": 2,
            "initial_state": {"black": "ccdd", "white": ""},
            "moves": [[4, 4]]
        });
        let view = game_view_from_json(&value, None).unwrap();
        // The first move was White's, so Black is to play next. OGS wire
        // coordinates count rows from the top.
        assert_eq!(view.board().get(4, 5), Some(Color::White));
        assert_eq!(view.to_play(), Color::Black);
    }

    #[test]
    fn applies_a_clock_push() {
        let mut view = game_view_from_json(&game_json(), None).unwrap();
        apply_clock_update(
            &mut view,
            &json!({"black_time": {"remaining": 120.5}, "current_player": 1}),
        );
        assert_eq!(
            view.clock.player(Color::Black).main,
            Duration::from_millis(120_500)
        );
        assert_eq!(view.clock.active(), Some(Color::Black));
    }

    #[test]
    fn estimate_toggle_and_confirm() {
        let mut view = game_view_from_json(&game_json(), None).unwrap();
        view.enter_estimate();
        assert!(view.estimate_active);
        // There are no stones to toggle at this point except the two present.
        assert!(view.toggle_estimate_dead(Coord::new(2, 7)));
        assert!(view.estimate_dead.contains(&(2, 7)));
        let summary = view.estimate_summary.clone().unwrap();
        assert_eq!(summary.dead_black, 1);
        assert!(view.toggle_estimate_dead(Coord::new(2, 7)));
        assert!(view.estimate_dead.is_empty());
        view.exit_estimate();
        assert!(!view.estimate_active);
        assert!(view.estimate_summary.is_none());
    }

    #[test]
    fn time_control_parsing() {
        assert_eq!(
            time_control_from_json(&json!({"main_time": 600, "time_control": "absolute"})),
            TimeControl::Absolute {
                main: Duration::from_secs(600)
            }
        );
        assert_eq!(
            time_control_from_json(
                &json!({"time_control": "fischer", "main_time": 300, "time_increment": 5})
            ),
            TimeControl::Fischer {
                main: Duration::from_secs(300),
                increment: Duration::from_secs(5)
            }
        );
        assert_eq!(
            time_control_from_json(&json!({
                "time_control": "byoyomi",
                "main_time": 600,
                "period_time": 30,
                "periods": 5
            })),
            TimeControl::Byoyomi {
                main: Duration::from_secs(600),
                periods: 5,
                period: Duration::from_secs(30)
            }
        );
        assert_eq!(time_control_from_json(&json!({})), TimeControl::None);
    }

    #[test]
    fn overview_and_game_list_parsing() {
        let mut store = Store::new("en");
        store.apply_overview(&json!({
            "active_games": [{
                "id": 5,
                "width": 13,
                "players": {"black": {"username": "a"}, "white": {"username": "b"}},
                "phase": "play"
            }],
            "challenges": [{"id": 9, "challenger": {"username": "c"}, "board_size": 9, "ranked": true}],
            "online": 321
        }));
        assert_eq!(store.lobby_games.len(), 1);
        assert_eq!(store.lobby_games[0].black, "a");
        assert_eq!(store.open_challenges.len(), 1);
        assert_eq!(store.online, Some(321));

        store.apply_game_list(&json!([{"id": 6, "players": {"black": "x", "white": "y"}}]));
        assert_eq!(store.lobby_games[0].id, 6);
        assert_eq!(store.lobby_games[0].black, "x");
    }

    #[test]
    fn notification_dedup_by_id() {
        let mut store = Store::new("en");
        store.push_notification(NotificationItem {
            id: "7".into(),
            text: "first".into(),
            at: None,
        });
        store.push_notification(NotificationItem {
            id: "7".into(),
            text: "updated".into(),
            at: None,
        });
        assert_eq!(store.notifications.len(), 1);
        assert_eq!(store.notifications[0].text, "updated");
    }

    #[test]
    fn parse_ogs_coords_formats() {
        // Array of arrays
        let v1 = json!([[3, 3], [4, 4]]);
        let coords1 = parse_ogs_coords(&v1, 19);
        assert_eq!(coords1.len(), 2);

        // Comma-separated GTP string
        let v2 = json!("D4, E5");
        let coords2 = parse_ogs_coords(&v2, 19);
        assert_eq!(coords2.len(), 2);
        assert_eq!(coords2[0].to_gtp(), "D4");
        assert_eq!(coords2[1].to_gtp(), "E5");

        // Compact SGF string
        let v3 = json!("ccee");
        let coords3 = parse_ogs_coords(&v3, 19);
        assert_eq!(coords3.len(), 2);

        // Empty string
        let v4 = json!("");
        assert!(parse_ogs_coords(&v4, 19).is_empty());
    }

    #[test]
    fn apply_removed_stones_string_and_empty() {
        let mut store = Store::new("en");
        let view = game_view_from_json(
            &json!({
                "game_id": 1,
                "width": 9,
                "height": 9,
                "phase": "stone removal",
                "players": {"black": {"username": "b"}, "white": {"username": "w"}},
            }),
            None,
        )
        .unwrap();
        store.games.insert(1, view);
        let game = store.game_mut(1).unwrap();
        apply_removed_stones(game, &json!({"removed_stones": "D4, E5"}));
        assert_eq!(game.scoring_dead.len(), 2);

        // Reset with empty string
        apply_removed_stones(game, &json!({"removed_stones": ""}));
        assert_eq!(game.scoring_dead.len(), 0);
    }
}
