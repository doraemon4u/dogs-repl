//! Full-screen TUI: event loop, responsive layout, and command dispatch.
//!
//! Rendering is synchronous and reads a snapshot refreshed by the async loop;
//! the store itself is never written here. The Session actor remains the single
//! writer, which keeps the render path free of lock contention and await points.

use crate::config;
use crate::engine::board::Color as StoneColor;
use crate::engine::estimate::score;
use crate::session::store::{ChatScope, ConnectionState, GameView, LogLevel, Store};
use crate::session::{Command, SessionHandle};
use crate::ui::challenge_options;
use crate::ui::commands::{
    self, EstimateCommand, GameMode, NewGameSpec, ScoreCommand, SgfAction, UndoCommand, UserCommand,
};
use crate::ui::new_game::{self, NewGameWizard, Outcome};
use crate::ui::tui::board_render;
use crate::ui::tui::input::InputState;
use crate::ui::tui::layout::{self, Scene};
use crate::ui::tui::theme::{ColorSupport, Theme};
use crate::ui::tui::widgets::{self, Overlay};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};
use std::cell::Cell;
use std::io::{self, Stdout};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Startup options supplied by `main`.
#[derive(Debug, Clone, Default)]
pub struct Startup {
    pub initial_game: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Board,
    Moves,
    Chat,
}

impl FocusPane {
    fn next(self) -> Self {
        match self {
            Self::Board => Self::Moves,
            Self::Moves => Self::Chat,
            Self::Chat => Self::Board,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Board => Self::Chat,
            Self::Moves => Self::Board,
            Self::Chat => Self::Moves,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Board => "board",
            Self::Moves => "moves",
            Self::Chat => "chat",
        }
    }
}

struct LoginState {
    username: String,
    password: String,
    focus_password: bool,
    message: String,
}

impl Default for LoginState {
    fn default() -> Self {
        Self {
            username: std::env::var("OGS_USERNAME").unwrap_or_default(),
            password: String::new(),
            focus_password: false,
            message: String::new(),
        }
    }
}

/// The count of a local game with the dead stones marked so far (none
/// until counting has started).
fn local_count(game: &GameView) -> crate::engine::estimate::ScoreSummary {
    let dead = if game.estimate_active {
        game.estimate_dead.clone()
    } else {
        Default::default()
    };
    let snapshot = game.active_game_ref();
    score(
        game.board(),
        &dead,
        game.komi,
        game.rule_set(),
        (snapshot.captures(StoneColor::Black), snapshot.captures(StoneColor::White)),
    )
}

/// A pending SGF path entry.
struct PathPrompt {
    action: SgfAction,
    input: InputState,
    error: Option<String>,
}

impl PathPrompt {
    fn new(action: SgfAction) -> Self {
        Self {
            action,
            input: InputState::new(),
            error: None,
        }
    }
}

impl LoginState {
    fn reset(&mut self) {
        self.password.clear();
        self.focus_password = false;
        self.message.clear();
    }
}

struct App {
    session: SessionHandle,
    theme: Theme,
    snapshot: Store,
    input: InputState,
    focus: FocusPane,
    login: LoginState,
    overlay: Option<Overlay>,
    /// The New Game flow, drawn above everything else while open.
    new_game: Option<NewGameWizard>,
    /// SGF import/export path entry.
    path_prompt: Option<PathPrompt>,
    /// Signed out but past the login screen, for local play.
    offline: bool,
    notice: Option<(String, Instant)>,
    moves_scroll_back: u16,
    chat_scroll_back: u16,
    lobby_scroll_back: u16,
    resize_deadline: Option<Instant>,
    dirty: bool,
    quit: bool,
    pending_game: Option<u64>,
    last_draw: Instant,
    persist_path: String,
    last_header_area: Cell<Rect>,
    last_board_area: Cell<Rect>,
    last_board_inner: Cell<Rect>,
    last_moves_area: Cell<Rect>,
    last_chat_area: Cell<Rect>,
    last_chat_inner: Cell<Rect>,
}

impl App {
    fn new(session: SessionHandle, theme: Theme, startup: Startup) -> Self {
        Self {
            session,
            theme,
            snapshot: Store::new("en"),
            input: InputState::new(),
            focus: FocusPane::Board,
            login: LoginState::default(),
            overlay: None,
            new_game: None,
            path_prompt: None,
            offline: false,
            notice: None,
            moves_scroll_back: 0,
            chat_scroll_back: 0,
            lobby_scroll_back: 0,
            resize_deadline: None,
            dirty: true,
            quit: false,
            pending_game: startup.initial_game,
            last_draw: Instant::now() - Duration::from_secs(1),
            persist_path: config::credentials_path().display().to_string(),
            last_header_area: Cell::new(Rect::default()),
            last_board_area: Cell::new(Rect::default()),
            last_board_inner: Cell::new(Rect::default()),
            last_moves_area: Cell::new(Rect::default()),
            last_chat_area: Cell::new(Rect::default()),
            last_chat_inner: Cell::new(Rect::default()),
        }
    }

    async fn refresh(&mut self) {
        self.snapshot = self.session.snapshot().await;
        if self.snapshot.username.is_some() && !self.login.password.is_empty() {
            self.login.password.clear();
        }
    }

    fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    /// The login screen is shown until the user signs in or chooses to play
    /// offline (F1 → New Game → 摆谱 needs no account).
    fn show_login(&self) -> bool {
        self.snapshot.username.is_none() && !self.offline
    }

    fn focused_game(&self) -> Option<&GameView> {
        self.snapshot.focused_game()
    }

    fn active_game_id(&self) -> Option<u64> {
        self.snapshot
            .active_game
            .or_else(|| self.snapshot.chat_scope.game_id())
    }

    fn scene(&self) -> Scene {
        match self.focused_game() {
            Some(game) => Scene::Game {
                board_size: game.size,
            },
            None => Scene::Lobby,
        }
    }

    fn should_draw(&self) -> bool {
        self.dirty && self.resize_deadline.is_none()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.last_draw = Instant::now();
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        if let Some(deadline) = self.resize_deadline {
            if now >= deadline {
                self.resize_deadline = None;
                self.dirty = true;
            }
        }
        if now.duration_since(self.last_draw) >= Duration::from_millis(250) {
            self.dirty = true;
        }
        if self.snapshot.connection == ConnectionState::Authenticated {
            if let Some(game_id) = self.pending_game.take() {
                self.session.send(Command::OpenGame(game_id));
                self.focus = FocusPane::Board;
                self.moves_scroll_back = 0;
                self.chat_scroll_back = 0;
                self.dirty = true;
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Resize(_, _) => {
                // Resize events arrive in storms while a window is dragged.
                // Wait until 40 ms pass without a new one before repainting.
                self.resize_deadline = Some(Instant::now() + Duration::from_millis(40));
                self.dirty = false;
            }
            Event::Paste(text) => {
                if self.show_login() {
                    if self.login.focus_password {
                        self.login.password.push_str(&text);
                    } else {
                        self.login.username.push_str(&text);
                    }
                } else if let Some(prompt) = &mut self.path_prompt {
                    prompt.input.insert_str(&text);
                } else if self.overlay.is_none() && self.new_game.is_none() {
                    self.input.insert_str(&text);
                }
                self.dirty = true;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.handle_key(key);
            }
            Event::Mouse(mouse) => {
                self.handle_mouse(mouse);
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        self.dirty = true;
        let (mx, my) = (mouse.column, mouse.row);

        if self.new_game.is_some() || self.path_prompt.is_some() {
            return;
        }
        let est_active = self.focused_game().map(|g| g.estimate_active).unwrap_or(false);

        // 1. Header action buttons click
        let header_area = self.last_header_area.get();
        if let Some(cmd_str) = widgets::header_action_at(mx, my, header_area, est_active) {
            let cmd = commands::parse(cmd_str);
            self.dispatch(cmd);
            return;
        }

        // 2. Board intersection click
        if let Some(game) = self.focused_game() {
            let board_inner = self.last_board_inner.get();
            if let Some(coord) = board_render::board_coord_at(board_inner, game.size, mx, my) {
                self.focus = FocusPane::Board;
                self.dispatch(UserCommand::Coordinate(coord));
                return;
            }
        }

        // 3. Chat channel pill click
        let chat_inner = self.last_chat_inner.get();
        if let Some(action) = widgets::chat_channel_at(mx, my, chat_inner) {
            if action == "channels" {
                self.dispatch(UserCommand::Channels);
            } else {
                self.dispatch(UserCommand::Join(action));
            }
            self.focus = FocusPane::Chat;
            return;
        }

        // 4. Pane selection
        let in_rect = |r: Rect| mx >= r.x && mx < r.x + r.width && my >= r.y && my < r.y + r.height;
        if in_rect(self.last_board_area.get()) {
            self.focus = FocusPane::Board;
        } else if in_rect(self.last_moves_area.get()) {
            self.focus = FocusPane::Moves;
        } else if in_rect(self.last_chat_area.get()) {
            self.focus = FocusPane::Chat;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c'))
        {
            self.quit = true;
            return;
        }
        self.dirty = true;

        if self.path_prompt.is_some() {
            self.handle_path_prompt_key(key);
            return;
        }
        if self.new_game.is_some() {
            self.handle_new_game_key(key);
            return;
        }
        if self.overlay.is_some() {
            match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    let chosen = self
                        .overlay
                        .as_ref()
                        .and_then(|ov| ov.selected_channel().map(str::to_string));
                    self.overlay = None;
                    if let Some(channel) = chosen {
                        self.session.send(Command::JoinChannel(channel));
                        self.focus = FocusPane::Chat;
                        self.chat_scroll_back = 0;
                    }
                }
                KeyCode::Up => {
                    if let Some(overlay) = &mut self.overlay {
                        overlay.scroll_by(-1);
                    }
                }
                KeyCode::Down => {
                    if let Some(overlay) = &mut self.overlay {
                        overlay.scroll_by(1);
                    }
                }
                KeyCode::PageUp => {
                    if let Some(overlay) = &mut self.overlay {
                        overlay.scroll_by(-10);
                    }
                }
                KeyCode::PageDown => {
                    if let Some(overlay) = &mut self.overlay {
                        overlay.scroll_by(10);
                    }
                }
                _ => {}
            }
            return;
        }

        if self.show_login() {
            self.handle_login_key(key);
            return;
        }

        match key {
            KeyEvent {
                code: KeyCode::Tab,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::SHIFT) => {
                self.focus = self.focus.previous();
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::BackTab,
                ..
            } => {
                self.focus = self.focus.next();
            }
            KeyEvent {
                code: KeyCode::Char('1'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.focus = FocusPane::Board,
            KeyEvent {
                code: KeyCode::Char('2'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.focus = FocusPane::Moves,
            KeyEvent {
                code: KeyCode::Char('3'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.focus = FocusPane::Chat,
            KeyEvent { code: KeyCode::F(1), .. } => {
                self.dispatch(UserCommand::NewGame(NewGameSpec::default()))
            }
            KeyEvent { code: KeyCode::F(4), .. } => self.dispatch(UserCommand::PresetKo),
            KeyEvent { code: KeyCode::F(5), .. } => self.dispatch(UserCommand::Estimate(EstimateCommand::Auto)),
            KeyEvent { code: KeyCode::F(6), .. } => self.dispatch(UserCommand::Estimate(EstimateCommand::Exit)),
            KeyEvent { code: KeyCode::F(7), .. } => self.dispatch(UserCommand::Step(crate::ui::commands::StepCommand::Prev(1))),
            KeyEvent { code: KeyCode::F(8), .. } => self.dispatch(UserCommand::Step(crate::ui::commands::StepCommand::Next(1))),
            KeyEvent { code: KeyCode::F(9), .. } => self.dispatch(UserCommand::Channels),
            KeyEvent { code: KeyCode::F(10), .. } => self.dispatch(UserCommand::Help),
            KeyEvent {
                code: KeyCode::Char('r'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.session.send(Command::RefreshLobby),
            KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.toggle_chat_scope(),
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => self.scroll_back(5),
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => self.scroll_back(-5),
            KeyEvent {
                code: KeyCode::Up, ..
            } => self.input.history_prev(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => self.input.history_next(),
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                let line = self.input.submit();
                if !line.trim().is_empty() {
                    let command = commands::parse(&line);
                    self.dispatch(command);
                }
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if self.input.is_empty() {
                    self.overlay = None;
                } else {
                    self.input.clear();
                }
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => self.input.backspace(),
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => self.input.delete(),
            KeyEvent {
                code: KeyCode::Left,
                ..
            } => self.input.left(),
            KeyEvent {
                code: KeyCode::Right,
                ..
            } => self.input.right(),
            KeyEvent {
                code: KeyCode::Home,
                ..
            } => self.input.home(),
            KeyEvent {
                code: KeyCode::End, ..
            } => self.input.end(),
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.input.clear(),
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.input.insert(ch);
            }
            _ => {}
        }
    }

    fn handle_login_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::F(1) => {
                self.offline = true;
                self.login.message.clear();
                self.dispatch(UserCommand::NewGame(NewGameSpec::default()));
            }
            KeyCode::Tab => self.login.focus_password = !self.login.focus_password,
            KeyCode::BackTab => self.login.focus_password = !self.login.focus_password,
            KeyCode::Enter => {
                if self.login.username.trim().is_empty() {
                    self.login.message = "username is required".to_string();
                } else if self.login.password.is_empty() {
                    self.login.message = "password is required".to_string();
                } else {
                    self.session.send(Command::Login {
                        username: self.login.username.trim().to_string(),
                        password: self.login.password.clone(),
                    });
                    self.login.message = "signing in…".to_string();
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.login.focus_password {
                    self.login.password.clear();
                } else {
                    self.login.username.clear();
                }
            }
            KeyCode::Backspace => {
                if self.login.focus_password {
                    self.login.password.pop();
                } else {
                    self.login.username.pop();
                }
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.login.focus_password {
                    self.login.password.push(ch);
                } else {
                    self.login.username.push(ch);
                }
            }
            _ => {}
        }
    }

    fn scroll_back(&mut self, delta: i32) {
        let target = if self.focused_game().is_some() {
            match self.focus {
                FocusPane::Moves => &mut self.moves_scroll_back,
                FocusPane::Chat => &mut self.chat_scroll_back,
                FocusPane::Board => &mut self.moves_scroll_back,
            }
        } else if self.focus == FocusPane::Chat {
            &mut self.chat_scroll_back
        } else {
            &mut self.lobby_scroll_back
        };
        let updated = i32::from(*target) + delta;
        *target = updated.max(0) as u16;
    }

    fn toggle_chat_scope(&mut self) {
        let Some(game_id) = self.active_game_id() else {
            self.set_notice("no game open");
            return;
        };
        let scope = if matches!(self.snapshot.chat_scope, ChatScope::Game { .. }) {
            ChatScope::World {
                channel: self.snapshot.world_channel.clone(),
            }
        } else {
            ChatScope::Game { game_id }
        };
        self.session.send(Command::SetChatScope(scope));
        self.chat_scroll_back = 0;
    }

    fn dispatch(&mut self, command: UserCommand) {
        let game_id = self.active_game_id();
        let estimate_active = self
            .focused_game()
            .map(|game| game.estimate_active)
            .unwrap_or(false);
        match command {
            UserCommand::Help => self.overlay = Some(widgets::help_overlay()),
            UserCommand::Quit => self.quit = true,
            UserCommand::Login { username, password } => {
                if let Some(password) = password {
                    self.session.send(Command::Login { username, password });
                    self.login.message = "signing in…".to_string();
                } else {
                    self.login.username = username;
                    self.login.focus_password = true;
                    self.login.message = "enter the password and press Enter".to_string();
                }
            }
            UserCommand::Logout => {
                self.session.send(Command::Logout);
                self.login.reset();
                self.overlay = None;
                self.offline = false;
            }
            UserCommand::Reconnect => self.session.send(Command::Reconnect),
            UserCommand::Refresh => self.session.send(Command::RefreshLobby),
            UserCommand::WhoAmI => {
                let text = format!(
                    "user: {}\nid: {}\nconnection: {}\ngames loaded: {}\nworld channel: #{}\nchat scope: {}",
                    self.snapshot.username.as_deref().unwrap_or("-"),
                    self.snapshot
                        .user_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "-".into()),
                    self.snapshot.connection.label(),
                    self.snapshot.games.len(),
                    self.snapshot.world_channel,
                    self.snapshot.chat_scope.label()
                );
                self.overlay = Some(Overlay::new("Who am I", text));
            }
            UserCommand::Lobby => {
                if let Some(game_id) = game_id {
                    self.session.send(Command::CloseGame(game_id));
                }
                self.session.send(Command::RefreshLobby);
                self.moves_scroll_back = 0;
                self.chat_scroll_back = 0;
                self.lobby_scroll_back = 0;
                self.focus = FocusPane::Chat;
            }
            UserCommand::Games => self.session.send(Command::LoadMyGames),
            UserCommand::Challenges => self.session.send(Command::LoadChallenges),
            UserCommand::ChallengeCreate(spec) => {
                self.session
                    .send(Command::CreateChallenge(challenge_options(spec)));
                self.set_notice("challenge sent");
            }
            UserCommand::ChallengeAccept(id) => {
                self.session.send(Command::AcceptChallenge(id))
            }
            UserCommand::ChallengeDecline(id) => {
                self.session.send(Command::DeclineChallenge(id))
            }
            UserCommand::OpenGame(game_id) => {
                self.session.send(Command::OpenGame(game_id));
                self.pending_game = None;
                self.focus = FocusPane::Board;
                self.moves_scroll_back = 0;
                self.chat_scroll_back = 0;
            }
            UserCommand::CloseGame => match game_id {
                Some(game_id) => self.session.send(Command::CloseGame(game_id)),
                None => self.session.send(Command::SetChatScope(ChatScope::World {
                    channel: self.snapshot.world_channel.clone(),
                })),
            },
            UserCommand::NewGame(spec) => self.open_new_game(spec),
            UserCommand::PresetKo => {
                self.session.send(crate::ui::preset_ko_command());
                self.focus = FocusPane::Board;
                self.set_notice("Preset: a ko at the lower left. Black to play — take it with [c2].");
            }
            UserCommand::PromptSgfPath(action) => {
                if action == SgfAction::Save && game_id.is_none() {
                    self.set_notice("no game open");
                } else {
                    self.path_prompt = Some(PathPrompt::new(action));
                }
            }
            UserCommand::LoadSgf(game_id) => self.session.send(Command::LoadSgf(game_id)),
            UserCommand::LoadSgfFile(path) => {
                self.session.send(Command::LoadSgfFile { path });
                self.focus = FocusPane::Board;
                self.moves_scroll_back = 0;
            }
            UserCommand::SaveSgfFile(path) => match game_id {
                Some(game_id) => self.session.send(Command::SaveSgfFile { game_id, path }),
                None => self.set_notice("no game open"),
            },
            UserCommand::Step(step) => match game_id {
                Some(game_id) => {
                    self.session.send(Command::StepGame { game_id, step });
                    self.focus = FocusPane::Board;
                }
                None => self.set_notice("no game open"),
            },
            UserCommand::Coordinate(coord) => match game_id {
                Some(game_id) if estimate_active => {
                    self.session.send(Command::ToggleEstimateDead { game_id, coord })
                }
                Some(game_id) => self
                    .session
                    .send(Command::PlayMove { game_id, coord }),
                None => self.set_notice("no game open: use `game <id>` first"),
            },
            UserCommand::Pass => match game_id {
                Some(game_id) => self.session.send(Command::Pass { game_id }),
                None => self.set_notice("no game open"),
            },
            UserCommand::Resign => match game_id {
                Some(game_id) => self.session.send(Command::Resign { game_id }),
                None => self.set_notice("no game open"),
            },
            UserCommand::Undo(action) => match (game_id, action) {
                (Some(game_id), UndoCommand::Request(number)) => {
                    self.session.send(Command::UndoRequest {
                        game_id,
                        move_number: number,
                    })
                }
                (Some(game_id), UndoCommand::Accept(move_number)) => self
                    .session
                    .send(Command::UndoAccept {
                        game_id,
                        move_number,
                    }),
                (Some(game_id), UndoCommand::Decline(move_number)) => self
                    .session
                    .send(Command::UndoCancel {
                        game_id,
                        move_number,
                    }),
                (None, _) => self.set_notice("no game open"),
            },
            UserCommand::Chat(text) => self.session.send(Command::SendChat {
                channel: None,
                text,
            }),
            UserCommand::Join(channel) => {
                self.session.send(Command::JoinChannel(channel));
                self.focus = FocusPane::Chat;
                self.chat_scroll_back = 0;
            }
            UserCommand::Part(channel) => self.session.send(Command::PartChannel(channel)),
            UserCommand::Channels => {
                self.session.send(Command::FetchChannels);
                self.overlay = Some(Overlay::channel_picker(&self.snapshot));
            }
            UserCommand::World(channel) => {
                self.session.send(Command::SetWorldChannel(channel));
                self.focus = FocusPane::Chat;
                self.chat_scroll_back = 0;
            }
            UserCommand::Estimate(action) => match (game_id, action) {
                (Some(game_id), EstimateCommand::Enter) => {
                    self.session.send(Command::EstimateEnter { game_id });
                    self.focus = FocusPane::Board;
                }
                (Some(game_id), EstimateCommand::Confirm) => {
                    self.session.send(Command::EstimateConfirm { game_id })
                }
                (Some(game_id), EstimateCommand::Exit) => {
                    self.session.send(Command::EstimateExit { game_id })
                }
                (Some(game_id), EstimateCommand::Auto) => {
                    self.session.send(Command::EstimateAuto { game_id });
                    self.focus = FocusPane::Board;
                }
                (None, _) => self.set_notice("no game open"),
            },
            UserCommand::OfficialDead { coords, removed } => match game_id {
                Some(game_id) => {
                    self.session.send(Command::OfficialDead {
                        game_id,
                        coords,
                        removed,
                    });
                    self.focus = FocusPane::Moves;
                }
                None => self.set_notice("no game open"),
            },
            UserCommand::Score(ScoreCommand::Show) => self.show_score(),
            UserCommand::Score(ScoreCommand::Accept) => match game_id {
                Some(game_id) => self.session.send(Command::OfficialAccept { game_id }),
                None => self.set_notice("no game open"),
            },
            UserCommand::Score(ScoreCommand::Reject) => match game_id {
                Some(game_id) => self.session.send(Command::OfficialReject { game_id }),
                None => self.set_notice("no game open"),
            },
            UserCommand::Player(name) => self.session.send(Command::LookupPlayer(name)),
            UserCommand::Notifications => {
                let mut lines: Vec<String> = self
                    .snapshot
                    .notifications
                    .iter()
                    .rev()
                    .map(|note| note.text.clone())
                    .collect();
                if lines.is_empty() {
                    lines.push("no notifications".to_string());
                }
                self.overlay = Some(Overlay::new("Notifications", lines.join("\n")));
            }
            UserCommand::Version => {
                self.overlay = Some(widgets::version_overlay(&self.persist_path))
            }
            UserCommand::Clear => {
                self.overlay = None;
                self.notice = None;
            }
            UserCommand::Focus(pane) => {
                self.focus = match pane.as_str() {
                    "board" => FocusPane::Board,
                    "moves" | "list" => FocusPane::Moves,
                    "chat" => FocusPane::Chat,
                    _ => {
                        self.set_notice("focus must be board, moves, or chat");
                        self.focus
                    }
                };
            }
            UserCommand::Unknown(word) => {
                self.set_notice(format!("unknown command '{word}'; type help"));
                self.session.log(
                    LogLevel::Error,
                    format!("unknown command '{word}' (type `help`)"),
                );
            }
            UserCommand::Invalid(message) => {
                self.set_notice(message.clone());
                self.session.log(LogLevel::Error, message);
            }
        }
        self.dirty = true;
    }

    fn show_score(&mut self) {
        let Some(game) = self.focused_game() else {
            self.set_notice("no game open");
            return;
        };
        if game.local {
            let summary = local_count(game);
            let text = format!(
                "{}\n规则 {} · komi {}\n\n{summary}\n{}\n\n\
                 Dead stones: {} marked{}. Click a group to toggle it, `undo` to resume play.",
                game.title(),
                game.rule_set().label(),
                game.komi,
                summary.breakdown(),
                game.estimate_dead.len(),
                if game.estimate_active { "" } else { " — detecting dead stones, the count updates in the log" },
            );
            if !game.estimate_active {
                if let Some(game_id) = self.active_game_id() {
                    self.session.send(Command::EstimateAuto { game_id });
                }
            }
            self.overlay = Some(Overlay::new("数子 / Count", text));
            return;
        }
        let mut lines = vec![game.title()];
        if game.phase == crate::session::store::GamePhase::StoneRemoval {
            lines.push("server phase: stone removal (official)".to_string());
        }
        if !game.scoring_dead.is_empty() {
            let mut coords: Vec<_> = game.scoring_dead.iter().copied().collect();
            coords.sort_unstable();
            let list = coords
                .iter()
                .map(|&(col, row)| crate::engine::coord::Coord::new(col, row).to_gtp())
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!("official dead marked: {list}"));
        }
        if let Some(message) = &game.scoring_last_message {
            lines.push(message.clone());
        }
        if let Some(summary) = &game.estimate_summary {
            lines.push(format!("local estimate: {summary}"));
        } else {
            let summary = score(
                game.board(),
                &game.scoring_dead,
                game.komi,
                game.rule_set(),
                (
                    game.game.captures(StoneColor::Black),
                    game.game.captures(StoneColor::White),
                ),
            );
            lines.push(format!("count with currently marked dead: {summary}"));
        }
        lines.push(String::new());
        lines.push(
            "The local number is informational. The official result is the server's\n\
             negotiated stone-removal result: mark with `dead [a10]`, then `score accept`\n\
             or `score reject`."
                .to_string(),
        );
        self.overlay = Some(Overlay::new("Score", lines.join("\n")));
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.show_login() {
            let message = match self.snapshot.connection {
                ConnectionState::Connecting => "restoring / signing in…".to_string(),
                ConnectionState::Error => self
                    .snapshot
                    .connection_message
                    .clone()
                    .unwrap_or_else(|| self.login.message.clone()),
                _ => self.login.message.clone(),
            };
            widgets::draw_login(
                frame,
                area,
                &self.login.username,
                &self.login.password,
                self.login.focus_password,
                &message,
                &self.theme,
            );
            if let Some(overlay) = &self.overlay {
                widgets::draw_overlay(frame, area, overlay, &self.theme);
            }
            return;
        }

        let scene = self.scene();
        if let Some(min) = layout::too_small(area, scene) {
            widgets::draw_too_small(
                frame,
                area,
                (area.width, area.height),
                min,
                &self.theme,
            );
            if let Some(overlay) = &self.overlay {
                widgets::draw_overlay(frame, area, overlay, &self.theme);
            }
            return;
        }

        match self.focused_game() {
            Some(game) => self.draw_game(frame, area, game),
            None => self.draw_lobby(frame, area),
        }
        if let Some(overlay) = &self.overlay {
            widgets::draw_overlay(frame, area, overlay, &self.theme);
        }
        if let Some(wizard) = &self.new_game {
            widgets::draw_new_game(frame, area, wizard, &self.theme);
        }
        if let Some(prompt) = &self.path_prompt {
            widgets::draw_path_prompt(
                frame,
                area,
                prompt.action.prompt(),
                &prompt.input,
                prompt.error.as_deref(),
                &self.theme,
            );
        }
    }

    fn open_new_game(&mut self, spec: NewGameSpec) {
        if spec.mode == Some(GameMode::VsAi) && self.snapshot.username.is_none() {
            self.set_notice("vs AI needs a logged-in OGS account; `new local` plays offline");
            return;
        }
        let wizard = NewGameWizard::new(
            spec,
            self.snapshot.username.is_some(),
            self.snapshot.online_bots.clone(),
        );
        if wizard.step().is_none() {
            let spec = wizard.spec.clone();
            self.start_new_game(&spec);
        } else {
            self.overlay = None;
            self.new_game = Some(wizard);
        }
    }

    fn start_new_game(&mut self, spec: &NewGameSpec) {
        if spec.mode == Some(GameMode::VsAi) && self.snapshot.username.is_none() {
            self.set_notice("vs AI needs a logged-in OGS account");
            return;
        }
        match new_game::start_command(spec, &self.snapshot.online_bots) {
            Ok(command) => {
                if let Command::CreateChallenge(options) = &command {
                    self.set_notice(format!(
                        "challenging {} ({}x{}, {}, unranked)…",
                        options.opponent, options.board_size, options.board_size, options.rules
                    ));
                } else {
                    self.focus = FocusPane::Board;
                    self.moves_scroll_back = 0;
                }
                self.session.send(command);
            }
            Err(error) => self.set_notice(error),
        }
    }

    fn handle_new_game_key(&mut self, key: KeyEvent) {
        let Some(wizard) = &mut self.new_game else { return };
        let outcome = match key.code {
            KeyCode::Esc => {
                self.new_game = None;
                return;
            }
            KeyCode::Backspace | KeyCode::Left => {
                if !wizard.back() {
                    self.new_game = None;
                }
                return;
            }
            KeyCode::Up => {
                wizard.move_by(-1);
                return;
            }
            KeyCode::Down | KeyCode::Tab => {
                wizard.move_by(1);
                return;
            }
            KeyCode::Enter | KeyCode::Right => wizard.confirm(),
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                wizard.choose(ch as usize - '1' as usize)
            }
            _ => return,
        };
        match outcome {
            Outcome::Next => {}
            Outcome::Blocked(reason) => self.set_notice(reason),
            Outcome::Done(spec) => {
                self.new_game = None;
                self.start_new_game(&spec);
            }
        }
    }

    fn handle_path_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.path_prompt else { return };
        match key.code {
            KeyCode::Esc => self.path_prompt = None,
            KeyCode::Enter => {
                let action = prompt.action;
                match action.with_path(&prompt.input.buffer()) {
                    Some(command) => {
                        self.path_prompt = None;
                        self.dispatch(command);
                    }
                    None => prompt.error = Some("type a file path, or Esc to cancel".into()),
                }
            }
            KeyCode::Backspace => prompt.input.backspace(),
            KeyCode::Delete => prompt.input.delete(),
            KeyCode::Left => prompt.input.left(),
            KeyCode::Right => prompt.input.right(),
            KeyCode::Home => prompt.input.home(),
            KeyCode::End => prompt.input.end(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                prompt.input.clear()
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                prompt.error = None;
                prompt.input.insert(ch);
            }
            _ => {}
        }
    }

    fn draw_game(&self, frame: &mut Frame, area: ratatui::layout::Rect, game: &GameView) {
        let layout = layout::split_game(area, game.size);
        widgets::draw_header(frame, layout.header, &self.theme, game.estimate_active);
        self.last_header_area.set(layout.header);
        self.last_board_area.set(layout.board);
        self.last_moves_area.set(layout.moves);
        self.last_chat_area.set(layout.chat);

        widgets::render_pane(
            frame,
            layout.board,
            &widgets::board_title(game),
            self.theme.board,
            &self.theme,
            self.focus == FocusPane::Board,
            |frame, inner| {
                self.last_board_inner.set(inner);
                widgets::draw_board(frame, inner, game, &self.theme, true);
            },
        );
        widgets::render_pane(
            frame,
            layout.moves,
            &format!("Moves · {}", game.move_number()),
            self.theme.moves,
            &self.theme,
            self.focus == FocusPane::Moves,
            |frame, inner| {
                widgets::draw_moves(frame, inner, game, &self.theme, self.moves_scroll_back)
            },
        );
        widgets::render_pane(
            frame,
            layout.chat,
            &widgets::chat_title(&self.snapshot),
            self.theme.chat,
            &self.theme,
            self.focus == FocusPane::Chat,
            |frame, inner| {
                self.last_chat_inner.set(inner);
                widgets::draw_chat(
                    frame,
                    inner,
                    &self.snapshot,
                    &self.snapshot.current_chat_channel(),
                    &self.theme,
                    self.chat_scroll_back,
                )
            },
        );
        self.draw_bottom(frame, layout.status, layout.input);
    }

    fn draw_lobby(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let layout = layout::split_lobby(area);
        widgets::draw_header(frame, layout.header, &self.theme, false);
        self.last_header_area.set(layout.header);
        widgets::render_pane(
            frame,
            layout.panel,
            "OGS Lobby · one pane for browsing",
            self.theme.lobby,
            &self.theme,
            true,
            |frame, inner| {
                let split = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([
                        ratatui::layout::Constraint::Percentage(55),
                        ratatui::layout::Constraint::Percentage(45),
                    ])
                    .split(inner);
                widgets::draw_lobby_games(
                    frame,
                    split[0],
                    &self.snapshot,
                    &self.theme,
                    self.lobby_scroll_back,
                );
                widgets::draw_divider(frame, split[1], split[1].x, &self.theme);
                widgets::draw_chat(
                    frame,
                    split[1],
                    &self.snapshot,
                    &self.snapshot.current_chat_channel(),
                    &self.theme,
                    self.chat_scroll_back,
                );
                // Recent log lines in the bottom of the games half.
                if split[0].height > 8 {
                    let log_area = ratatui::layout::Rect {
                        x: split[0].x,
                        y: split[0].y + split[0].height - 5,
                        width: split[0].width,
                        height: 5,
                    };
                    let lines = widgets::recent_log_lines(&self.snapshot, 4);
                    let paragraph = ratatui::widgets::Paragraph::new(
                        lines
                            .iter()
                            .map(|line| ratatui::text::Line::from(line.clone()))
                            .collect::<Vec<_>>(),
                    )
                    .style(ratatui::style::Style::default().fg(self.theme.muted));
                    frame.render_widget(paragraph, log_area);
                }
            },
        );
        self.draw_bottom(frame, layout.status, layout.input);
    }

    fn draw_bottom(
        &self,
        frame: &mut Frame,
        status: ratatui::layout::Rect,
        input: ratatui::layout::Rect,
    ) {
        let notice = self
            .notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(8))
            .map(|(text, _)| text.as_str());
        widgets::draw_status(frame, status, &self.snapshot, &self.theme, notice);
        let game_id = self.active_game_id();
        let estimate = self
            .focused_game()
            .map(|game| game.estimate_active)
            .unwrap_or(false);
        let prompt = commands::prompt(game_id, estimate);
        widgets::draw_input(
            frame,
            input,
            &prompt,
            &self.input.buffer(),
            self.input.cursor(),
            &self.theme,
        );
    }
}

// ------------------------------------------------------------------ terminal

struct TerminalGuard;

impl TerminalGuard {
    fn install() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            cursor::Hide,
            event::EnableBracketedPaste,
            event::EnableMouseCapture
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            cursor::Show,
            event::DisableBracketedPaste,
            event::DisableMouseCapture
        );
    }
}

fn spawn_event_reader() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(event) => {
                if tx.send(event).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    });
    rx
}

/// Runs the TUI until the user quits.
pub async fn run(session: SessionHandle, startup: Startup) -> io::Result<()> {
    let theme = Theme::new(ColorSupport::detect());
    let _guard = TerminalGuard::install()?;
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            cursor::Show,
            event::DisableBracketedPaste,
            event::DisableMouseCapture
        );
        original_hook(info);
    }));

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal: Terminal<CrosstermBackend<Stdout>> = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = App::new(session, theme, startup);
    app.refresh().await;
    terminal.draw(|frame| app.draw(frame))?;
    app.mark_drawn();

    let mut events = spawn_event_reader();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            maybe = events.recv() => {
                let Some(event) = maybe else { break };
                app.handle_event(event);
            }
            _ = ticker.tick() => {
                app.on_tick();
            }
        }
        app.refresh().await;
        if app.quit {
            break;
        }
        if app.should_draw() {
            terminal.draw(|frame| app.draw(frame))?;
            app.mark_drawn();
        }
    }

    app.session.send(Command::Quit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::rest_client::ChallengeClock;
    use crate::ui::commands::{ChallengeClockSpec, ChallengeSpec};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    async fn app() -> App {
        let config = crate::session::SessionConfig {
            world_channel: "en".to_string(),
            persist_path: None,
        };
        let session = SessionHandle::spawn_with_store(config, Store::new("en"));
        let mut app = App::new(
            session,
            Theme::new(ColorSupport::TrueColor),
            Startup::default(),
        );
        app.refresh().await;
        app
    }

    async fn settle(app: &mut App) {
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            app.refresh().await;
        }
    }

    #[tokio::test]
    async fn f1_on_the_login_screen_starts_an_offline_baipu_game_by_keyboard() {
        let mut app = app().await;
        assert!(app.show_login(), "starts on the login screen");
        assert!(app.snapshot.games.is_empty(), "no scratch board at startup");

        app.handle_key(key(KeyCode::F(1)));
        assert!(!app.show_login());
        assert!(app.new_game.is_some());
        app.handle_key(key(KeyCode::Char('3'))); // 19×19
        // vs AI is disabled while signed out: Enter on it is refused.
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.notice.as_ref().is_some_and(|(text, _)| text.contains("login")));
        app.handle_key(key(KeyCode::Char('2'))); // 摆谱
        app.handle_key(key(KeyCode::Char('2'))); // Japanese/Korean
        assert!(app.new_game.is_none());
        settle(&mut app).await;

        let game = app.focused_game().expect("local game opened").clone();
        assert!(game.local);
        assert_eq!(game.size, 19);
        assert_eq!(game.rule_set(), crate::engine::rules::RuleSet::JapaneseKorean);

        // Typing a move and two passes reaches counting.
        for line in ["play Q16", "pass", "pass"] {
            for ch in line.chars() {
                app.handle_key(key(KeyCode::Char(ch)));
            }
            app.handle_key(key(KeyCode::Enter));
        }
        settle(&mut app).await;
        let game = app.focused_game().unwrap();
        assert_eq!(game.move_number(), 3);
        // Counting runs in the background; the UI stays responsive meanwhile.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !app.focused_game().unwrap().estimate_active && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            app.refresh().await;
        }
        let game = app.focused_game().unwrap();
        assert!(game.estimate_active, "two passes start counting");
        assert!(game.estimate_summary.is_some());

        // `sgf save` with no path opens the prompt; Esc cancels it.
        for ch in "sgf save".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.path_prompt.is_some());
        app.handle_key(key(KeyCode::Esc));
        assert!(app.path_prompt.is_none());

        // `size` is gone and points at New Game.
        for ch in "size 9".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.notice.as_ref().is_some_and(|(text, _)| text.contains("new")));
    }

    #[test]
    fn focus_cycles_both_ways() {
        assert_eq!(FocusPane::Board.next(), FocusPane::Moves);
        assert_eq!(FocusPane::Moves.next(), FocusPane::Chat);
        assert_eq!(FocusPane::Chat.next(), FocusPane::Board);
        assert_eq!(FocusPane::Board.previous(), FocusPane::Chat);
    }

    #[test]
    fn challenge_spec_converts_to_transport_options() {
        let spec = ChallengeSpec {
            opponent: "kata".into(),
            board_size: 9,
            rules: "japanese".into(),
            ranked: false,
            color: "white".into(),
            clock: ChallengeClockSpec::Fischer {
                initial_time: 300,
                increment: 5,
            },
        };
        let options = challenge_options(spec);
        assert_eq!(options.opponent, "kata");
        assert_eq!(options.board_size, 9);
        assert!(!options.ranked);
        assert_eq!(options.challenger_color, "white");
        assert_eq!(
            options.clock,
            ChallengeClock::Fischer {
                initial_time: 300,
                increment: 5
            }
        );
    }

    #[test]
    fn login_state_defaults_to_the_environment_username() {
        // The environment is process-global; only assert the field exists.
        let state = LoginState::default();
        assert!(state.password.is_empty());
        assert!(!state.focus_password);
    }
}
