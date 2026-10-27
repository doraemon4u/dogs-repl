//! Plain-text REPL (phase 4a): the end-to-end loop without TUI rendering.
//!
//! It shares the command grammar and the Session actor with the TUI, so the two
//! front ends cannot drift apart. Network events and the board are printed as
//! they arrive; the prompt is redrawn after updates.

use crate::engine::board::Color;
use crate::engine::estimate::{score, ScoringRules};
use crate::session::store::{ChatKind, ConnectionState, GamePhase, GameView, LogLevel, Store};
use crate::session::{Command, SessionHandle};
use crate::ui::commands::{
    self, EstimateCommand, ScoreCommand, UndoCommand, UserCommand,
};
use crate::ui::tui::{board_render, estimate_render};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

/// REPL startup options.
#[derive(Debug, Clone, Default)]
pub struct ReplOptions {
    pub initial_game: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoardSignature {
    game_id: u64,
    moves: u16,
    displayed_move: u16,
    estimate: bool,
    dead: usize,
    phase: GamePhase,
}

impl BoardSignature {
    fn of(game: &GameView) -> Self {
        Self {
            game_id: game.id,
            moves: game.move_number(),
            displayed_move: game.displayed_move_number(),
            estimate: game.estimate_active,
            dead: game.estimate_dead.len(),
            phase: game.phase,
        }
    }
}

struct Repl {
    session: SessionHandle,
    last_log_seq: u64,
    last_chat_seq: u64,
    last_connection: ConnectionState,
    last_chat_channel: String,
    board_signature: Option<BoardSignature>,
    pending_login: Option<String>,
    pending_initial_game: Option<u64>,
    lobby_deadline: Option<Instant>,
    prompt_shown: bool,
    quit: bool,
}

impl Repl {
    fn new(session: SessionHandle, options: ReplOptions) -> Self {
        Self {
            session,
            last_log_seq: 0,
            last_chat_seq: 0,
            last_connection: ConnectionState::Offline,
            last_chat_channel: String::new(),
            board_signature: None,
            pending_login: None,
            pending_initial_game: options.initial_game,
            lobby_deadline: None,
            prompt_shown: false,
            quit: false,
        }
    }

    async fn banner(&mut self) {
        let store = self.session.snapshot().await;
        println!(
            "ogs-tui {} — text REPL (type `help`, `quit` to exit)",
            env!("CARGO_PKG_VERSION")
        );
        match &store.username {
            Some(name) => println!("signed in as {name} (id {})", store.user_id.unwrap_or(0)),
            None => println!(
                "not signed in. Use `login <user> <password>` (or set OGS_USERNAME/OGS_PASSWORD), \
                 or wait for a saved session to resume."
            ),
        }
        println!(
            "world #{}, board {}x{} coordinate syntax [a10]\n",
            store.world_channel,
            store
                .focused_game()
                .map(|game| game.size)
                .unwrap_or(19),
            store
                .focused_game()
                .map(|game| game.size)
                .unwrap_or(19)
        );
        // Show a little history, then treat it as already seen.
        let current = store.current_chat_channel();
        if let Some(channel) = store.channels.get(&current) {
            for line in channel.messages.iter().rev().take(5).rev() {
                print_chat_line(line);
            }
        }
        self.last_log_seq = store.log_seq;
        self.last_chat_seq = store.chat_seq;
        self.last_connection = store.connection;
        self.last_chat_channel = current;
    }

    /// Prints anything new from the store. Cheap; called on every tick.
    async fn tick(&mut self) {
        let store = self.session.snapshot().await;
        if store.connection != self.last_connection {
            if store.connection != ConnectionState::Offline {
                println!(
                    "[socket] {}",
                    store
                        .connection_message
                        .as_deref()
                        .unwrap_or(store.connection.label())
                );
            }
            self.last_connection = store.connection;
        }

        for line in &store.logs {
            if line.seq > self.last_log_seq {
                print_log_line(line);
                self.last_log_seq = line.seq;
            }
        }

        self.print_new_chat(&store);
        self.print_board_if_changed(&store);

        if self.pending_initial_game.is_some()
            && store.connection == ConnectionState::Authenticated
        {
            if let Some(game_id) = self.pending_initial_game.take() {
                self.session.send(Command::OpenGame(game_id));
                println!("opening game {game_id}…");
            }
        }
        if let Some(deadline) = self.lobby_deadline {
            if Instant::now() >= deadline {
                self.lobby_deadline = None;
                print_lobby(&store);
            }
        }
        if !self.prompt_shown {
            self.print_prompt(&store);
        }
    }

    fn print_new_chat(&mut self, store: &Store) {
        let current = store.current_chat_channel();
        if current != self.last_chat_channel {
            // Scope changed: show recent context for the new channel and mark
            // everything already delivered elsewhere as seen.
            println!("[chat] switched to {}", store.chat_scope.label());
            if let Some(channel) = store.channels.get(&current) {
                for line in channel.messages.iter().rev().take(8).rev() {
                    print_chat_line(line);
                }
            }
            self.last_chat_channel = current.clone();
            self.last_chat_seq = store.chat_seq;
            return;
        }
        let mut unseen: HashMap<String, usize> = HashMap::new();
        for (name, channel) in &store.channels {
            for line in &channel.messages {
                if line.seq <= self.last_chat_seq {
                    continue;
                }
                self.last_chat_seq = self.last_chat_seq.max(line.seq);
                if *name == current {
                    print_chat_line(line);
                } else {
                    *unseen.entry(name.clone()).or_default() += 1;
                }
            }
        }
        for (name, count) in unseen {
            println!("[unread] #{name}: {count} new message(s)");
        }
    }

    fn print_board_if_changed(&mut self, store: &Store) {
        let Some(game) = store.focused_game() else {
            self.board_signature = None;
            return;
        };
        let signature = BoardSignature::of(game);
        if self.board_signature == Some(signature) {
            return;
        }
        self.board_signature = Some(signature);
        println!();
        println!("{}", game.title());
        if game.estimate_active {
            println!("{}", estimate_render::render_text(game));
        } else {
            println!("{}", board_render::render_text(game));
        }
        let control = game.clock.control();
        let black = game.clock.player(Color::Black);
        let white = game.clock.player(Color::White);
        println!(
            "clock  B {}{}   W {}{}",
            black.display(control),
            if game.clock.active() == Some(Color::Black) {
                " *"
            } else {
                ""
            },
            white.display(control),
            if game.clock.active() == Some(Color::White) {
                " *"
            } else {
                ""
            }
        );
        println!(
            "phase {} · {} · {}x{} · komi {} · captures B{} W{}",
            game.phase.label(),
            game.our_color
                .map(|color| format!("you are {color:?}"))
                .unwrap_or_else(|| "spectating".to_string()),
            game.size,
            game.size,
            game.komi,
            game.game.captures(Color::Black),
            game.game.captures(Color::White)
        );
    }

    fn print_prompt(&mut self, store: &Store) {
        if let Some(username) = &self.pending_login {
            let _ = username;
            print!("password> ");
        } else {
            let game_id = store.active_game.or_else(|| store.chat_scope.game_id());
            let estimate = store
                .focused_game()
                .map(|game| game.estimate_active)
                .unwrap_or(false);
            print!("{}", commands::prompt(game_id, estimate));
        }
        let _ = io::stdout().flush();
        self.prompt_shown = true;
    }

    async fn handle_line(&mut self, line: String) -> bool {
        self.prompt_shown = false;
        if let Some(username) = self.pending_login.take() {
            let password = line.trim().to_string();
            if password.is_empty() {
                println!("[error] empty password");
                return true;
            }
            self.session.send(Command::Login { username, password });
            println!("[..] signing in…");
            return true;
        }
        let command = commands::parse(&line);
        self.dispatch(command).await
    }

    async fn dispatch(&mut self, command: UserCommand) -> bool {
        let store = self.session.snapshot().await;
        let game_id = store.active_game.or_else(|| store.chat_scope.game_id());
        let estimate_active = store
            .focused_game()
            .map(|game| game.estimate_active)
            .unwrap_or(false);
        match command {
            UserCommand::Quit => return false,
            UserCommand::Help => println!("{}", commands::help_text()),
            UserCommand::Clear => {
                print!("\x1b[2J\x1b[H");
                let _ = io::stdout().flush();
            }
            UserCommand::Version => println!("ogs-tui {}", env!("CARGO_PKG_VERSION")),
            UserCommand::WhoAmI => {
                println!(
                    "user {}, id {}, connection {}, {} game(s) loaded",
                    store.username.as_deref().unwrap_or("-"),
                    store
                        .user_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "-".into()),
                    store.connection.label(),
                    store.games.len()
                );
            }
            UserCommand::Notifications => {
                if store.notifications.is_empty() {
                    println!("no notifications");
                } else {
                    for note in store.notifications.iter().rev() {
                        println!("[note] {}", note.text);
                    }
                }
            }
            UserCommand::Channels => {
                self.session.send(Command::FetchChannels);
                println!("active world channel: #{}", store.world_channel);
                println!("joined channels:");
                for (name, channel) in &store.channels {
                    if channel.joined {
                        println!("  #{name} ({} messages)", channel.messages.len());
                    }
                }
                if !store.available_channels.is_empty() {
                    println!("available: {}", store.available_channels.join(", "));
                }
                println!("tip: use `channels <name>` or `join <name>` to enter a channel");
            }
            UserCommand::Focus(_) => println!(
                "[i] focus panes are a TUI concept; the REPL prints panes sequentially"
            ),
            UserCommand::Score(ScoreCommand::Show) => print_score(&store),
            UserCommand::Score(ScoreCommand::Accept) => match game_id {
                Some(game_id) => self.session.send(Command::OfficialAccept { game_id }),
                None => println!("[!] no game open"),
            },
            UserCommand::Score(ScoreCommand::Reject) => match game_id {
                Some(game_id) => self.session.send(Command::OfficialReject { game_id }),
                None => println!("[!] no game open"),
            },
            UserCommand::Estimate(EstimateCommand::Auto) => match game_id {
                Some(game_id) => self.session.send(Command::EstimateAuto { game_id }),
                None => println!("[!] no game open"),
            },
            UserCommand::Login { username, password } => match password {
                Some(password) => {
                    self.session.send(Command::Login { username, password });
                    println!("[..] signing in…");
                }
                None => {
                    if let Ok(password) = std::env::var("OGS_PASSWORD") {
                        self.session.send(Command::Login { username, password });
                        println!("[..] signing in with OGS_PASSWORD…");
                    } else {
                        self.pending_login = Some(username);
                        println!("password will be echoed; set OGS_PASSWORD to avoid this");
                    }
                }
            },
            UserCommand::Logout => {
                self.session.send(Command::Logout);
                println!("[i] signed out");
            }
            UserCommand::Lobby => {
                if let Some(game_id) = game_id {
                    self.session.send(Command::CloseGame(game_id));
                }
                self.session.send(Command::RefreshLobby);
                self.lobby_deadline = Some(Instant::now() + Duration::from_millis(900));
            }
            UserCommand::Games => {
                self.session.send(Command::LoadMyGames);
                self.lobby_deadline = Some(Instant::now() + Duration::from_millis(900));
            }
            UserCommand::Challenges => {
                self.session.send(Command::LoadChallenges);
                self.lobby_deadline = Some(Instant::now() + Duration::from_millis(900));
            }
            UserCommand::Coordinate(coord) => match game_id {
                Some(game_id) if estimate_active => {
                    self.session.send(Command::ToggleEstimateDead { game_id, coord })
                }
                Some(game_id) => self.session.send(Command::PlayMove { game_id, coord }),
                None => println!("[!] no game open; use `game <id>` first"),
            },
            UserCommand::Pass => match game_id {
                Some(game_id) => self.session.send(Command::Pass { game_id }),
                None => println!("[!] no game open"),
            },
            UserCommand::Resign => match game_id {
                Some(game_id) => self.session.send(Command::Resign { game_id }),
                None => println!("[!] no game open"),
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
                (None, _) => println!("[!] no game open"),
            },
            UserCommand::Chat(text) => self.session.send(Command::SendChat {
                channel: None,
                text,
            }),
            UserCommand::Join(channel) => self.session.send(Command::JoinChannel(channel)),
            UserCommand::Part(channel) => self.session.send(Command::PartChannel(channel)),
            UserCommand::World(channel) => self.session.send(Command::SetWorldChannel(channel)),
            UserCommand::OpenGame(game_id) => {
                self.session.send(Command::OpenGame(game_id));
                println!("[..] opening game {game_id}…");
            }
            UserCommand::CloseGame => match game_id {
                Some(game_id) => self.session.send(Command::CloseGame(game_id)),
                None => println!("[!] no game open"),
            },
            UserCommand::BoardSize(size) => {
                self.session.send(Command::SetBoardSize { game_id, size });
            }
            UserCommand::PresetKo => {
                let gid = game_id.unwrap_or(42);
                self.session
                    .send(Command::SetBoardSize { game_id: Some(gid), size: 9 });
                let demo_moves = [
                    crate::engine::coord::Coord::new(1, 9),
                    crate::engine::coord::Coord::new(2, 8),
                    crate::engine::coord::Coord::new(0, 8),
                    crate::engine::coord::Coord::new(1, 7),
                    crate::engine::coord::Coord::new(1, 8),
                    crate::engine::coord::Coord::new(0, 9),
                ];
                for coord in demo_moves {
                    self.session.send(Command::PlayMove { game_id: gid, coord });
                }
                println!("[i] Preset: Ko shape ready at A8/A9. Try capturing [a8]!");
            }
            UserCommand::LoadSgf(game_id) => {
                self.session.send(Command::LoadSgf(game_id));
                println!("[..] loading SGF for game {game_id}…");
            }
            UserCommand::LoadSgfFile(path) => {
                self.session.send(Command::LoadSgfFile { path });
            }
            UserCommand::SaveSgfFile(path) => match game_id {
                Some(game_id) => self.session.send(Command::SaveSgfFile { game_id, path }),
                None => println!("[!] no game open"),
            },
            UserCommand::Step(step) => match game_id {
                Some(game_id) => self.session.send(Command::StepGame { game_id, step }),
                None => println!("[!] no game open"),
            },
            UserCommand::Estimate(action) => match (game_id, action) {
                (Some(game_id), EstimateCommand::Enter) => {
                    self.session.send(Command::EstimateEnter { game_id })
                }
                (Some(game_id), EstimateCommand::Confirm) => {
                    self.session.send(Command::EstimateConfirm { game_id })
                }
                (Some(game_id), EstimateCommand::Exit) => {
                    self.session.send(Command::EstimateExit { game_id })
                }
                (None, _) => println!("[!] no game open"),
                _ => {}
            },
            UserCommand::OfficialDead { coords, removed } => match game_id {
                Some(game_id) => self.session.send(Command::OfficialDead {
                    game_id,
                    coords,
                    removed,
                }),
                None => println!("[!] no game open"),
            },
            UserCommand::ChallengeCreate(spec) => {
                self.session
                    .send(Command::CreateChallenge(crate::ui::challenge_options(spec)));
                println!("[..] sending challenge…");
            }
            UserCommand::ChallengeAccept(id) => {
                self.session.send(Command::AcceptChallenge(id))
            }
            UserCommand::ChallengeDecline(id) => {
                self.session.send(Command::DeclineChallenge(id))
            }
            UserCommand::Player(name) => self.session.send(Command::LookupPlayer(name)),
            UserCommand::Reconnect => self.session.send(Command::Reconnect),
            UserCommand::Refresh => self.session.send(Command::RefreshLobby),
            UserCommand::Unknown(word) => {
                println!("[error] unknown command '{word}' (type `help`)");
            }
            UserCommand::Invalid(message) => println!("[error] {message}"),
        }
        true
    }
}

fn print_log_line(line: &crate::session::store::LogLine) {
    let prefix = match line.level {
        LogLevel::Info => "[..]",
        LogLevel::Success => "[ok]",
        LogLevel::Warning => "[!]",
        LogLevel::Error => "[error]",
    };
    println!("{prefix} {}", line.text);
}

fn print_chat_line(line: &crate::session::store::ChatLine) {
    match line.kind {
        ChatKind::System => println!("  · {}", line.text),
        ChatKind::Chat => println!("  {}: {}", line.author, line.text),
    }
}

fn print_lobby(store: &Store) {
    println!(
        "\nLobby: online {} · active {} · your games {} · challenges {}",
        store
            .online
            .map(|online| online.to_string())
            .unwrap_or_else(|| "?".to_string()),
        store.lobby_games.len(),
        store.my_games.len(),
        store.my_challenges.len()
    );
    if !store.lobby_games.is_empty() {
        println!("Active games:");
        for game in store.lobby_games.iter().take(10) {
            println!("  {}", game.label());
        }
    }
    if !store.my_games.is_empty() {
        println!("Your games:");
        for game in store.my_games.iter().take(10) {
            println!("  {}", game.label());
        }
    }
    if !store.my_challenges.is_empty() {
        println!("Challenges:");
        for challenge in store.my_challenges.iter().take(10) {
            println!(
                "  #{} from {} ({}x{}, {}{})",
                challenge.id,
                challenge.challenger,
                challenge.board_size,
                challenge.board_size,
                challenge.rules,
                if challenge.ranked { ", ranked" } else { "" }
            );
        }
    }
    if !store.notifications.is_empty() {
        println!("Notifications:");
        for note in store.notifications.iter().rev().take(5) {
            println!("  {}", note.text);
        }
    }
}

fn print_score(store: &Store) {
    let Some(game) = store.focused_game() else {
        println!("[!] no game open");
        return;
    };
    println!("{}", game.title());
    println!("phase: {}", game.phase.label());
    if !game.scoring_dead.is_empty() {
        let mut coords: Vec<_> = game.scoring_dead.iter().copied().collect();
        coords.sort_unstable();
        let list = coords
            .iter()
            .map(|&(col, row)| crate::engine::coord::Coord::new(col, row).to_gtp())
            .collect::<Vec<_>>()
            .join(" ");
        println!("official dead marked: {list}");
    }
    if let Some(message) = &game.scoring_last_message {
        println!("server scoring: {message}");
    }
    let summary = score(
        game.board(),
        &game.scoring_dead,
        game.komi,
        ScoringRules::from_ogs(&game.rules),
        (
            game.game.captures(Color::Black),
            game.game.captures(Color::White),
        ),
    );
    println!("count with current official marks: {summary}");
    if let Some(local) = &game.estimate_summary {
        println!("local estimate: {local}");
    }
    println!(
        "Official result is negotiated: `dead [a10]…`, then `score accept` or `score reject`."
    );
}

fn spawn_stdin_reader() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        loop {
            let mut line = String::new();
            match lock.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

/// Runs the text REPL until EOF or `quit`.
pub async fn run(session: SessionHandle, options: ReplOptions) -> io::Result<()> {
    let mut repl = Repl::new(session, options);
    repl.banner().await;
    let mut lines = spawn_stdin_reader();
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    repl.tick().await;
    loop {
        tokio::select! {
            maybe = lines.recv() => {
                let Some(line) = maybe else { break };
                if !repl.handle_line(line).await {
                    break;
                }
                repl.tick().await;
            }
            _ = ticker.tick() => {
                repl.tick().await;
            }
        }
        if repl.quit {
            break;
        }
    }
    repl.session.send(Command::Quit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::coord::Coord;

    fn store_with_game() -> Store {
        let mut store = Store::new("en");
        let mut game = GameView::empty(7, 9);
        game.black = "alice".into();
        game.white = "bob".into();
        game.game.play(Coord::new(2, 2)).unwrap();
        game.game.play(Coord::new(6, 6)).unwrap();
        store.games.insert(7, game);
        store.switch_to_game(7);
        store
    }

    #[test]
    fn board_signature_changes_with_moves_and_estimate() {
        let mut game = GameView::empty(1, 9);
        let first = BoardSignature::of(&game);
        game.game.play(Coord::new(2, 2)).unwrap();
        assert_ne!(first, BoardSignature::of(&game));
        game.enter_estimate();
        assert_ne!(BoardSignature::of(&game), first);
    }

    #[test]
    fn chat_sequence_is_monotonic_across_channels() {
        let mut store = store_with_game();
        store.push_chat("en", "a", "one");
        store.push_chat("game/7", "b", "two");
        store.push_system_chat("game/7", "system");
        let seqs: Vec<u64> = store
            .channels
            .values()
            .flat_map(|channel| channel.messages.iter().map(|line| line.seq))
            .collect();
        let max = *seqs.iter().max().unwrap();
        assert_eq!(max, store.chat_seq);
        assert_eq!(seqs.len(), 3);
    }
}
