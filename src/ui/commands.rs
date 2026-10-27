//! REPL command grammar shared by the text REPL and the TUI input line.
//!
//! Parsing is deliberately independent of the session: it turns a line into a
//! [`UserCommand`], and the presentation dispatcher decides which actor command
//! that maps to. Coordinates use the tolerant `[a10]` syntax from design §7.2;
//! the estimate mode reuses the same parser.

use crate::engine::coord::{parse_coord, Coord};

#[derive(Debug, Clone, PartialEq)]
pub enum UserCommand {
    Help,
    Quit,
    Login {
        username: String,
        password: Option<String>,
    },
    Logout,
    Reconnect,
    Refresh,
    WhoAmI,
    Lobby,
    Games,
    Challenges,
    ChallengeCreate(ChallengeSpec),
    ChallengeAccept(i64),
    ChallengeDecline(i64),
    OpenGame(u64),
    CloseGame,
    BoardSize(u8),
    LoadSgf(u64),
    LoadSgfFile(String),
    SaveSgfFile(String),
    Step(StepCommand),
    Coordinate(Coord),
    Pass,
    Resign,
    Undo(UndoCommand),
    Chat(String),
    Join(String),
    Part(String),
    Channels,
    World(String),
    PresetKo,
    Estimate(EstimateCommand),
    OfficialDead {
        coords: Vec<Coord>,
        removed: bool,
    },
    Score(ScoreCommand),
    Player(String),
    Notifications,
    Version,
    Clear,
    Focus(String),
    Unknown(String),
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepCommand {
    Prev(u16),
    Next(u16),
    First,
    Last,
    Goto(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoCommand {
    Request(Option<u64>),
    Accept(u64),
    Decline(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateCommand {
    Enter,
    Confirm,
    Exit,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreCommand {
    Show,
    Accept,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeSpec {
    pub opponent: String,
    pub board_size: u8,
    pub rules: String,
    pub ranked: bool,
    pub color: String,
    pub clock: ChallengeClockSpec,
}

impl Default for ChallengeSpec {
    fn default() -> Self {
        Self {
            opponent: String::new(),
            board_size: 19,
            rules: "chinese".to_string(),
            ranked: true,
            color: "automatic".to_string(),
            clock: ChallengeClockSpec::Byoyomi {
                main_time: 600,
                period_time: 30,
                periods: 5,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChallengeClockSpec {
    Byoyomi {
        main_time: u32,
        period_time: u32,
        periods: u32,
    },
    Fischer {
        initial_time: u32,
        increment: u32,
    },
    Absolute {
        main_time: u32,
    },
}

impl ChallengeClockSpec {
    fn default_for(kind: &str) -> Self {
        match kind {
            "fischer" => Self::Fischer {
                initial_time: 300,
                increment: 5,
            },
            "absolute" => Self::Absolute { main_time: 600 },
            _ => Self::Byoyomi {
                main_time: 600,
                period_time: 30,
                periods: 5,
            },
        }
    }
}

/// Parses one input line.
pub fn parse(input: &str) -> UserCommand {
    let input = input.trim();
    if input.is_empty() {
        return UserCommand::Unknown(String::new());
    }
    if input.starts_with('[') {
        return match parse_coord(input) {
            Ok(coord) => UserCommand::Coordinate(coord),
            Err(error) => UserCommand::Invalid(format!("{error}: {input}")),
        };
    }
    let parts: Vec<&str> = input.split_whitespace().collect();
    let keyword = parts[0].to_ascii_lowercase();
    match keyword.as_str() {
        "help" | "h" | "?" => UserCommand::Help,
        "quit" | "exit" | "q" | ":q" => UserCommand::Quit,
        "login" => match parts.get(1) {
            Some(username) => UserCommand::Login {
                username: (*username).to_string(),
                password: parts.get(2).map(|password| (*password).to_string()),
            },
            None => UserCommand::Invalid("usage: login <username> [password]".into()),
        },
        "logout" => UserCommand::Logout,
        "connect" | "reconnect" => UserCommand::Reconnect,
        "refresh" => UserCommand::Refresh,
        "whoami" | "me" => UserCommand::WhoAmI,
        "lobby" => UserCommand::Lobby,
        "games" | "mygames" => UserCommand::Games,
        "challenges" => UserCommand::Challenges,
        "challenge" => parse_challenge(&parts),
        "accept" => parse_id(parts.get(1)).map_or_else(
            || UserCommand::Invalid("usage: accept <challenge_id>".into()),
            UserCommand::ChallengeAccept,
        ),
        "decline" => parse_id(parts.get(1)).map_or_else(
            || UserCommand::Invalid("usage: decline <challenge_id>".into()),
            UserCommand::ChallengeDecline,
        ),
        "game" | "g" => parse_id(parts.get(1)).map_or_else(
            || UserCommand::Invalid("usage: game <game_id>".into()),
            |id| UserCommand::OpenGame(id as u64),
        ),
        "close" | "leave" => UserCommand::CloseGame,
        "sgf" => parse_sgf(input, &parts),
        "step" => parse_step(&parts),
        "prev" | "back" => {
            let count = parts
                .get(1)
                .and_then(|n| n.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            UserCommand::Step(StepCommand::Prev(count))
        }
        "next" | "forward" => {
            let count = parts
                .get(1)
                .and_then(|n| n.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            UserCommand::Step(StepCommand::Next(count))
        }
        "first" | "start" => UserCommand::Step(StepCommand::First),
        "last" | "end" | "live" => UserCommand::Step(StepCommand::Last),
        "goto" => match parts.get(1).and_then(|n| n.parse::<u16>().ok()) {
            Some(move_number) => UserCommand::Step(StepCommand::Goto(move_number)),
            None => UserCommand::Invalid("usage: goto <move_number>".into()),
        },
        "pass" => UserCommand::Pass,
        "resign" => UserCommand::Resign,
        "undo" => parse_undo(&parts),
        "chat" | "say" => match rest_after(input, parts[0]) {
            Some(text) if !text.is_empty() => UserCommand::Chat(text.to_string()),
            _ => UserCommand::Invalid("usage: chat <message>".into()),
        },
        "join" | "channel" => parse_channel(parts.get(1), keyword.as_str()),
        "part" | "leavechannel" => parse_channel(parts.get(1), "part"),
        "channels" => match parts.get(1) {
            None => UserCommand::Channels,
            Some(&"list") => UserCommand::Channels,
            Some(&"join") | Some(&"enter") => parse_channel(parts.get(2), "join"),
            Some(&"part") | Some(&"leave") => parse_channel(parts.get(2), "part"),
            Some(ch) => UserCommand::match_channel("join", ch.trim_start_matches('#')),
        },
        "world" => parse_channel(parts.get(1), "world"),
        "preset" => match parts.get(1).map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("ko") | Some("eye") | None => UserCommand::PresetKo,
            Some(other) => UserCommand::Invalid(format!("unknown preset '{other}' (available: ko)")),
        },
        "size" | "board" | "new" => match parts.get(1).and_then(|raw| raw.parse::<u8>().ok()) {
            Some(size) if [9, 13, 19].contains(&size) => UserCommand::BoardSize(size),
            _ => UserCommand::Invalid("usage: size <9|13|19> (supported board sizes: 9, 13, 19)".into()),
        },
        "estimate" | "est" => match parts.get(1).map(|value| value.to_ascii_lowercase()) {
            None => UserCommand::Estimate(EstimateCommand::Enter),
            Some(action) => match action.as_str() {
                "confirm" | "score" => UserCommand::Estimate(EstimateCommand::Confirm),
                "exit" | "off" | "quit" => UserCommand::Estimate(EstimateCommand::Exit),
                "auto" => UserCommand::Estimate(EstimateCommand::Auto),
                other => UserCommand::Invalid(format!(
                    "unknown estimate action '{other}' (confirm | exit | auto)"
                )),
            },
        },
        "dead" | "dame" | "alive" => parse_dead(&parts, keyword.as_str()),
        "score" => {
            if parts.len() == 1 {
                UserCommand::Score(ScoreCommand::Show)
            } else {
                match parts[1].to_ascii_lowercase().as_str() {
                    "show" | "status" => UserCommand::Score(ScoreCommand::Show),
                    "accept" | "ok" => UserCommand::Score(ScoreCommand::Accept),
                    "reject" | "dispute" | "decline" => UserCommand::Score(ScoreCommand::Reject),
                    other => UserCommand::Invalid(format!(
                        "unknown score action '{other}' (show | accept | reject)"
                    )),
                }
            }
        }
        "player" | "user" => match parts.get(1) {
            Some(name) => UserCommand::Player((*name).to_string()),
            None => UserCommand::Invalid("usage: player <username|id>".into()),
        },
        "notifications" | "notes" => UserCommand::Notifications,
        "version" | "ver" => UserCommand::Version,
        "clear" | "cls" => UserCommand::Clear,
        "focus" => match parts.get(1) {
            Some(pane) => UserCommand::Focus((*pane).to_ascii_lowercase()),
            None => UserCommand::Invalid("usage: focus <board|moves|chat|input>".into()),
        },
        "play" | "move" => match parts.get(1) {
            Some(token) => match parse_coord(token).or_else(|_| Coord::from_gtp(token)) {
                Ok(coord) => UserCommand::Coordinate(coord),
                Err(error) => UserCommand::Invalid(format!("{error}: {token}")),
            },
            None => UserCommand::Invalid("usage: play <coord>".into()),
        },
        _ => UserCommand::Unknown(parts[0].to_string()),
    }
}

fn parse_sgf(input: &str, parts: &[&str]) -> UserCommand {
    let Some(first) = parts.get(1) else {
        return UserCommand::Invalid(
            "usage: sgf <game_id> | sgf load <path> | sgf save <path>".into(),
        );
    };
    match first.to_ascii_lowercase().as_str() {
        "load" | "open" | "import" => {
            let path = rest_after(input, parts[0])
                .and_then(|rest| rest_after(rest, parts[1]))
                .map(str::trim)
                .unwrap_or_default();
            if path.is_empty() {
                UserCommand::Invalid("usage: sgf load <path>".into())
            } else {
                UserCommand::LoadSgfFile(path.to_string())
            }
        }
        "save" | "export" | "write" => {
            let path = rest_after(input, parts[0])
                .and_then(|rest| rest_after(rest, parts[1]))
                .map(str::trim)
                .unwrap_or_default();
            if path.is_empty() {
                UserCommand::Invalid("usage: sgf save <path>".into())
            } else {
                UserCommand::SaveSgfFile(path.to_string())
            }
        }
        _ => {
            if let Some(id) = parse_id(Some(first)).filter(|&id| id > 0) {
                UserCommand::LoadSgf(id as u64)
            } else if first.ends_with(".sgf") || first.contains('/') || first.contains('\\') {
                let path = rest_after(input, parts[0]).map(str::trim).unwrap_or(first);
                UserCommand::LoadSgfFile(path.to_string())
            } else {
                UserCommand::Invalid(
                    "usage: sgf <game_id> | sgf load <path> | sgf save <path>".into(),
                )
            }
        }
    }
}

fn parse_step(parts: &[&str]) -> UserCommand {
    let Some(arg) = parts.get(1) else {
        return UserCommand::Step(StepCommand::Next(1));
    };
    match arg.to_ascii_lowercase().as_str() {
        "prev" | "back" | "-1" => {
            let count = parts
                .get(2)
                .and_then(|n| n.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            UserCommand::Step(StepCommand::Prev(count))
        }
        "next" | "forward" | "+1" => {
            let count = parts
                .get(2)
                .and_then(|n| n.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            UserCommand::Step(StepCommand::Next(count))
        }
        "first" | "start" => UserCommand::Step(StepCommand::First),
        "last" | "end" | "live" => UserCommand::Step(StepCommand::Last),
        other => match other.parse::<i32>() {
            Ok(delta) if arg.starts_with('-') => {
                UserCommand::Step(StepCommand::Prev(delta.unsigned_abs() as u16))
            }
            Ok(delta) if arg.starts_with('+') => {
                UserCommand::Step(StepCommand::Next(delta.unsigned_abs() as u16))
            }
            Ok(move_number) if move_number >= 0 => {
                UserCommand::Step(StepCommand::Goto(move_number as u16))
            }
            _ => UserCommand::Invalid(
                "usage: step [prev [n] | next [n] | first | last | <move_number>]".into(),
            ),
        },
    }
}

fn parse_id(token: Option<&&str>) -> Option<i64> {
    token.and_then(|token| token.trim_start_matches('#').parse::<i64>().ok())
}

fn parse_channel(token: Option<&&str>, command: &str) -> UserCommand {
    match token {
        Some(channel) => UserCommand::match_channel(command, channel.trim_start_matches('#')),
        None => UserCommand::Invalid(format!("usage: {command} <channel>")),
    }
}

impl UserCommand {
    fn match_channel(command: &str, channel: &str) -> Self {
        if channel.is_empty() {
            return UserCommand::Invalid(format!("usage: {command} <channel>"));
        }
        match command {
            "join" | "channel" => Self::Join(channel.to_string()),
            "part" => Self::Part(channel.to_string()),
            _ => Self::World(channel.to_string()),
        }
    }
}

fn rest_after<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(prefix)?;
    Some(rest.trim_start())
}

fn parse_undo(parts: &[&str]) -> UserCommand {
    match parts.get(1).map(|value| value.to_ascii_lowercase()) {
        None => UserCommand::Undo(UndoCommand::Request(None)),
        Some(action) => match action.as_str() {
            "request" | "req" | "ask" => {
                UserCommand::Undo(UndoCommand::Request(parts.get(2).and_then(|n| n.parse().ok())))
            }
            "accept" | "ok" => match parts.get(2).and_then(|n| n.parse::<u64>().ok()) {
                Some(move_number) => UserCommand::Undo(UndoCommand::Accept(move_number)),
                None => UserCommand::Invalid("usage: undo accept <move_number>".into()),
            },
            "decline" | "cancel" | "reject" => match parts.get(2).and_then(|n| n.parse::<u64>().ok())
            {
                Some(move_number) => UserCommand::Undo(UndoCommand::Decline(move_number)),
                None => UserCommand::Invalid("usage: undo decline <move_number>".into()),
            },
            number if number.parse::<u64>().is_ok() => {
                UserCommand::Undo(UndoCommand::Request(number.parse().ok()))
            }
            other => UserCommand::Invalid(format!(
                "unknown undo action '{other}' (request | accept | decline)"
            )),
        },
    }
}

fn parse_dead(parts: &[&str], keyword: &str) -> UserCommand {
    let removed = keyword != "dame";
    let mut coords = Vec::new();
    for token in &parts[1..] {
        let parsed = parse_coord(token).or_else(|_| Coord::from_gtp(token));
        match parsed {
            Ok(coord) => coords.push(coord),
            Err(error) => return UserCommand::Invalid(format!("{error}: {token}")),
        }
    }
    if coords.is_empty() {
        return UserCommand::Invalid(format!("usage: {keyword} <coord> [coord ...]"));
    }
    UserCommand::OfficialDead { coords, removed }
}

fn parse_challenge(parts: &[&str]) -> UserCommand {
    match parts.get(1).map(|value| value.to_ascii_lowercase()) {
        Some(action) if action == "accept" => parse_id(parts.get(2))
            .map_or_else(|| UserCommand::Invalid("usage: challenge accept <id>".into()), UserCommand::ChallengeAccept),
        Some(action) if action == "decline" => parse_id(parts.get(2))
            .map_or_else(|| UserCommand::Invalid("usage: challenge decline <id>".into()), UserCommand::ChallengeDecline),
        Some(action) if action == "create" => parse_challenge_create(&parts[2..]),
        _ => UserCommand::Invalid(
            "usage: challenge create --opponent <name> [--board 9|13|19] [--rules chinese|japanese] \
             [--ranked|--unranked] [--color black|white|automatic] [--time byoyomi|fischer|absolute] \
             [--main 600] [--period 30] [--periods 5] [--increment 5]; or challenge accept|decline <id>"
                .into(),
        ),
    }
}

fn require<'a>(flag: &str, value: Option<&'a str>) -> Result<&'a str, UserCommand> {
    value.ok_or_else(|| UserCommand::Invalid(format!("{flag} needs a value")))
}

fn parse_challenge_create(args: &[&str]) -> UserCommand {
    let mut spec = ChallengeSpec::default();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index];
        let value = args.get(index + 1).copied();
        let result: Result<(), UserCommand> = (|| {
            match flag {
                "--opponent" | "-o" => spec.opponent = require(flag, value)?.to_string(),
                "--board" | "-b" => {
                    let raw = require(flag, value)?;
                    match raw.parse::<u8>() {
                        Ok(size) if [9, 13, 19].contains(&size) => spec.board_size = size,
                        _ => {
                            return Err(UserCommand::Invalid(format!(
                                "board size must be 9, 13, or 19 (got {raw})"
                            )))
                        }
                    }
                }
                "--rules" | "-r" => {
                    let raw = require(flag, value)?.to_ascii_lowercase();
                    if !["chinese", "japanese", "aga", "korean", "ing"].contains(&raw.as_str()) {
                        return Err(UserCommand::Invalid(format!(
                            "unknown rules '{raw}' (chinese | japanese | aga | korean | ing)"
                        )));
                    }
                    spec.rules = raw;
                }
                "--ranked" => spec.ranked = true,
                "--unranked" | "--casual" => spec.ranked = false,
                "--color" | "-c" => {
                    let raw = require(flag, value)?.to_ascii_lowercase();
                    if !["black", "white", "automatic"].contains(&raw.as_str()) {
                        return Err(UserCommand::Invalid(format!(
                            "color must be black, white, or automatic (got {raw})"
                        )));
                    }
                    spec.color = raw;
                }
                "--time" | "-t" => {
                    let raw = require(flag, value)?.to_ascii_lowercase();
                    if !["byoyomi", "fischer", "absolute"].contains(&raw.as_str()) {
                        return Err(UserCommand::Invalid(format!(
                            "time control must be byoyomi, fischer, or absolute (got {raw})"
                        )));
                    }
                    spec.clock = ChallengeClockSpec::default_for(&raw);
                }
                "--main" => {
                    let seconds = require(flag, value)?.parse::<u32>().map_err(|_| {
                        UserCommand::Invalid("--main must be seconds".into())
                    })?;
                    spec.clock = match spec.clock {
                        ChallengeClockSpec::Byoyomi {
                            period_time,
                            periods,
                            ..
                        } => ChallengeClockSpec::Byoyomi {
                            main_time: seconds,
                            period_time,
                            periods,
                        },
                        ChallengeClockSpec::Fischer { increment, .. } => {
                            ChallengeClockSpec::Fischer {
                                initial_time: seconds,
                                increment,
                            }
                        }
                        ChallengeClockSpec::Absolute { .. } => {
                            ChallengeClockSpec::Absolute { main_time: seconds }
                        }
                    };
                }
                "--period" => {
                    let seconds = require(flag, value)?.parse::<u32>().map_err(|_| {
                        UserCommand::Invalid("--period must be seconds".into())
                    })?;
                    if let ChallengeClockSpec::Byoyomi {
                        main_time, periods, ..
                    } = spec.clock
                    {
                        spec.clock = ChallengeClockSpec::Byoyomi {
                            main_time,
                            period_time: seconds,
                            periods,
                        };
                    } else {
                        return Err(UserCommand::Invalid(
                            "--period only applies to byoyomi".into(),
                        ));
                    }
                }
                "--periods" => {
                    let periods = require(flag, value)?.parse::<u32>().map_err(|_| {
                        UserCommand::Invalid("--periods must be an integer".into())
                    })?;
                    if let ChallengeClockSpec::Byoyomi {
                        main_time,
                        period_time,
                        ..
                    } = spec.clock
                    {
                        spec.clock = ChallengeClockSpec::Byoyomi {
                            main_time,
                            period_time,
                            periods,
                        };
                    } else {
                        return Err(UserCommand::Invalid(
                            "--periods only applies to byoyomi".into(),
                        ));
                    }
                }
                "--increment" => {
                    let increment = require(flag, value)?.parse::<u32>().map_err(|_| {
                        UserCommand::Invalid("--increment must be seconds".into())
                    })?;
                    if let ChallengeClockSpec::Fischer { initial_time, .. } = spec.clock {
                        spec.clock = ChallengeClockSpec::Fischer {
                            initial_time,
                            increment,
                        };
                    } else {
                        return Err(UserCommand::Invalid(
                            "--increment only applies to fischer".into(),
                        ));
                    }
                }
                other => {
                    return Err(UserCommand::Invalid(format!(
                        "unknown challenge option '{other}'"
                    )))
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            return error;
        }
        // Flags with values consume two tokens.
        let consumes_value = matches!(
            flag,
            "--opponent"
                | "-o"
                | "--board"
                | "-b"
                | "--rules"
                | "-r"
                | "--color"
                | "-c"
                | "--time"
                | "-t"
                | "--main"
                | "--period"
                | "--periods"
                | "--increment"
        );
        index += if consumes_value { 2 } else { 1 };
    }
    if spec.opponent.is_empty() {
        return UserCommand::Invalid(
            "challenge create needs --opponent <username>".into(),
        );
    }
    UserCommand::ChallengeCreate(spec)
}

/// The help text shown by `help` in both front ends.
pub fn help_text() -> &'static str {
    "OGS commands
  login <user> [password] | logout | connect | refresh | whoami
  lobby | games | challenges | player <name|id> | notifications
  game <id> | close | size <9|13|19> | sgf <id> | sgf load <path> | sgf save <path>
  step [prev|next|first|last|<move>] | prev [n] | next [n] | first | last | goto <move>
  [a10] | play D4 | pass | resign | undo [request|accept|decline] [move]
  chat <text> | channels [channel] | join <channel> | part <channel> | world <channel>
  challenge create --opponent <name> [--board 9|13|19] [--rules ...]
      [--ranked|--unranked] [--color black|white|automatic]
      [--time byoyomi|fischer|absolute] [--main s] [--period s] [--periods n] [--increment s]
  challenge accept|decline <id>
  estimate | estimate auto | estimate confirm | estimate exit
  dead <coord>... | alive <coord>... | dame <coord>...
  score show | score accept | score reject
  focus <board|moves|chat|input> | clear | version | quit"
}

/// The prompt shown by the text REPL.
pub fn prompt(game_id: Option<u64>, estimate: bool) -> String {
    match (game_id, estimate) {
        (Some(id), true) => format!("ogs[{id}|estimate]> "),
        (Some(id), false) => format!("ogs[{id}]> "),
        (None, _) => "ogs> ".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_commands() {
        assert_eq!(parse("help"), UserCommand::Help);
        assert_eq!(parse("quit"), UserCommand::Quit);
        assert_eq!(parse("  "), UserCommand::Unknown(String::new()));
        assert_eq!(
            parse("login alice secret"),
            UserCommand::Login {
                username: "alice".into(),
                password: Some("secret".into())
            }
        );
        assert!(matches!(
            parse("login alice"),
            UserCommand::Login { password: None, .. }
        ));
        assert!(matches!(
            parse("login"),
            UserCommand::Invalid(_)
        ));
    }

    #[test]
    fn parses_coordinates_and_moves() {
        assert_eq!(
            parse("[a10]"),
            UserCommand::Coordinate(Coord::new(0, 10))
        );
        assert_eq!(
            parse("[a          10]"),
            UserCommand::Coordinate(Coord::new(0, 10))
        );
        assert_eq!(
            parse("play D4"),
            UserCommand::Coordinate(Coord::new(3, 4))
        );
        assert_eq!(
            parse("[I10]"),
            UserCommand::Invalid("column letter I is skipped in Go notation: [I10]".into())
        );
        assert!(matches!(parse("[ab]"), UserCommand::Invalid(_)));
    }

    #[test]
    fn chat_keeps_embedded_spaces() {
        assert_eq!(
            parse("chat hello   there"),
            UserCommand::Chat("hello   there".into())
        );
        assert!(matches!(parse("chat"), UserCommand::Invalid(_)));
    }

    #[test]
    fn parses_game_and_sgf_ids() {
        assert_eq!(parse("game 1234"), UserCommand::OpenGame(1234));
        assert_eq!(parse("g #42"), UserCommand::OpenGame(42));
        assert!(matches!(parse("game"), UserCommand::Invalid(_)));
        assert_eq!(parse("sgf 99"), UserCommand::LoadSgf(99));
        assert_eq!(
            parse("sgf load /tmp/test.sgf"),
            UserCommand::LoadSgfFile("/tmp/test.sgf".into())
        );
        assert_eq!(
            parse("sgf save /tmp/out.sgf"),
            UserCommand::SaveSgfFile("/tmp/out.sgf".into())
        );
        assert_eq!(
            parse("sgf game.sgf"),
            UserCommand::LoadSgfFile("game.sgf".into())
        );
    }

    #[test]
    fn parses_step_and_navigation_commands() {
        assert_eq!(
            parse("step"),
            UserCommand::Step(StepCommand::Next(1))
        );
        assert_eq!(
            parse("step prev 3"),
            UserCommand::Step(StepCommand::Prev(3))
        );
        assert_eq!(
            parse("step next 2"),
            UserCommand::Step(StepCommand::Next(2))
        );
        assert_eq!(
            parse("step first"),
            UserCommand::Step(StepCommand::First)
        );
        assert_eq!(
            parse("step last"),
            UserCommand::Step(StepCommand::Last)
        );
        assert_eq!(
            parse("step 15"),
            UserCommand::Step(StepCommand::Goto(15))
        );
        assert_eq!(
            parse("prev 2"),
            UserCommand::Step(StepCommand::Prev(2))
        );
        assert_eq!(
            parse("next"),
            UserCommand::Step(StepCommand::Next(1))
        );
        assert_eq!(
            parse("first"),
            UserCommand::Step(StepCommand::First)
        );
        assert_eq!(
            parse("last"),
            UserCommand::Step(StepCommand::Last)
        );
        assert_eq!(
            parse("goto 8"),
            UserCommand::Step(StepCommand::Goto(8))
        );
    }

    #[test]
    fn parses_undo_forms() {
        assert_eq!(
            parse("undo"),
            UserCommand::Undo(UndoCommand::Request(None))
        );
        assert_eq!(
            parse("undo request 12"),
            UserCommand::Undo(UndoCommand::Request(Some(12)))
        );
        assert_eq!(
            parse("undo accept 12"),
            UserCommand::Undo(UndoCommand::Accept(12))
        );
        assert_eq!(
            parse("undo decline 4"),
            UserCommand::Undo(UndoCommand::Decline(4))
        );
        assert!(matches!(parse("undo accept"), UserCommand::Invalid(_)));
    }

    #[test]
    fn parses_estimate_commands() {
        assert_eq!(
            parse("estimate"),
            UserCommand::Estimate(EstimateCommand::Enter)
        );
        assert_eq!(
            parse("estimate confirm"),
            UserCommand::Estimate(EstimateCommand::Confirm)
        );
        assert_eq!(
            parse("estimate exit"),
            UserCommand::Estimate(EstimateCommand::Exit)
        );
        assert_eq!(
            parse("estimate auto"),
            UserCommand::Estimate(EstimateCommand::Auto)
        );
        assert!(matches!(
            parse("estimate nope"),
            UserCommand::Invalid(_)
        ));
    }

    #[test]
    fn parses_official_dead_stone_commands() {
        assert_eq!(
            parse("dead [a10] D4"),
            UserCommand::OfficialDead {
                coords: vec![Coord::new(0, 10), Coord::new(3, 4)],
                removed: true
            }
        );
        assert_eq!(
            parse("dame [b2]"),
            UserCommand::OfficialDead {
                coords: vec![Coord::new(1, 2)],
                removed: false
            }
        );
        assert!(matches!(parse("dead"), UserCommand::Invalid(_)));
    }

    #[test]
    fn parses_challenge_defaults_and_flags() {
        let command = parse("challenge create -o kata");
        let UserCommand::ChallengeCreate(spec) = command else {
            panic!("unexpected: {command:?}");
        };
        assert_eq!(spec.opponent, "kata");
        assert_eq!(spec.board_size, 19);
        assert_eq!(spec.rules, "chinese");
        assert!(spec.ranked);
        assert_eq!(
            spec.clock,
            ChallengeClockSpec::Byoyomi {
                main_time: 600,
                period_time: 30,
                periods: 5
            }
        );

        let command = parse(
            "challenge create --opponent Kata --board 9 --rules japanese --unranked \
             --color white --time fischer --main 300 --increment 10",
        );
        let UserCommand::ChallengeCreate(spec) = command else {
            panic!("unexpected: {command:?}");
        };
        assert_eq!(spec.board_size, 9);
        assert_eq!(spec.rules, "japanese");
        assert!(!spec.ranked);
        assert_eq!(spec.color, "white");
        assert_eq!(
            spec.clock,
            ChallengeClockSpec::Fischer {
                initial_time: 300,
                increment: 10
            }
        );
    }

    #[test]
    fn rejects_bad_challenge_flags() {
        assert!(matches!(
            parse("challenge create"),
            UserCommand::Invalid(_)
        ));
        assert!(matches!(
            parse("challenge create -o x --board 10"),
            UserCommand::Invalid(_)
        ));
        assert!(matches!(
            parse("challenge create -o x --rules foo"),
            UserCommand::Invalid(_)
        ));
        assert!(matches!(
            parse("challenge create -o x --time absolute --period 20"),
            UserCommand::Invalid(_)
        ));
        assert!(matches!(
            parse("challenge create -o x --main"),
            UserCommand::Invalid(_)
        ));
    }

    #[test]
    fn parses_channels_and_score() {
        assert_eq!(parse("join #en"), UserCommand::Join("en".into()));
        assert_eq!(parse("channels #zh"), UserCommand::Join("zh".into()));
        assert_eq!(parse("channels join ja"), UserCommand::Join("ja".into()));
        assert_eq!(parse("channel ko"), UserCommand::Join("ko".into()));
        assert_eq!(parse("part zh"), UserCommand::Part("zh".into()));
        assert_eq!(parse("world ja"), UserCommand::World("ja".into()));
        assert_eq!(parse("size 9"), UserCommand::BoardSize(9));
        assert_eq!(parse("board 13"), UserCommand::BoardSize(13));
        assert_eq!(parse("new 19"), UserCommand::BoardSize(19));
        assert!(matches!(parse("size 10"), UserCommand::Invalid(_)));
        assert_eq!(parse("score"), UserCommand::Score(ScoreCommand::Show));
        assert_eq!(
            parse("score accept"),
            UserCommand::Score(ScoreCommand::Accept)
        );
        assert_eq!(
            parse("score reject"),
            UserCommand::Score(ScoreCommand::Reject)
        );
    }

    #[test]
    fn unknown_words_are_reported() {
        assert_eq!(
            parse("frobnicate"),
            UserCommand::Unknown("frobnicate".into())
        );
    }

    #[test]
    fn prompts_reflect_game_and_estimate_state() {
        assert_eq!(prompt(None, false), "ogs> ");
        assert_eq!(prompt(Some(42), false), "ogs[42]> ");
        assert_eq!(prompt(Some(42), true), "ogs[42|estimate]> ");
    }
}

