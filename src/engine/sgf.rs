//! SGF (Smart Game Format) reader and writer.
//!
//! The reader understands the parts of FF[4] that OGS actually emits: the
//! header properties, setup stones (`AB`/`AW`), main-line moves, passes, and
//! escaped property text. Variations are consumed and validated but only the
//! first variation is replayed.
//!
//! Coordinates inside this module follow SGF exactly: columns are 0-based
//! `a..`, rows are 0-based from the **top** of the board. The Engine's
//! [`Coord`] uses rows from the bottom, so conversion happens at the boundary.

use super::board::{Board, Color};
use super::clock::TimeControl;
use super::coord::Coord;
use super::game::Game;

/// A single move in an SGF game tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SgfMove {
    pub color: Color,
    /// Column (0-based), or `None` for pass.
    pub col: Option<u8>,
    /// Row (0-based from the top), or `None` for pass.
    pub row: Option<u16>,
}

impl SgfMove {
    /// Builds an SGF move from an Engine coordinate.
    pub fn from_coord(color: Color, coord: Coord, board_size: u8) -> Self {
        Self {
            color,
            col: Some(coord.col),
            row: Some(board_size.saturating_sub(coord.row as u8) as u16),
        }
    }

    /// Converts this move to an Engine coordinate. Returns `None` for passes
    /// or coordinates outside a board of `board_size`.
    pub fn to_coord(self, board_size: u8) -> Option<Coord> {
        let (col, row_from_top) = (self.col?, self.row?);
        if col >= board_size || row_from_top >= board_size as u16 {
            return None;
        }
        Some(Coord::new(
            col,
            board_size as u16 - row_from_top,
        ))
    }
}

/// A parsed SGF game.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SgfGame {
    pub board_size: u8,
    pub moves: Vec<SgfMove>,
    /// Setup stones placed before the first move.
    pub setup_black: Vec<(u8, u16)>,
    pub setup_white: Vec<(u8, u16)>,
    /// Komi value (points given to White for going second).
    pub komi: f64,
    pub player_black: Option<String>,
    pub player_white: Option<String>,
    pub black_rank: Option<String>,
    pub white_rank: Option<String>,
    pub game_name: Option<String>,
    pub event: Option<String>,
    pub round: Option<String>,
    pub handicap: Option<u8>,
    pub main_time_secs: Option<u32>,
    pub overtime: Option<String>,
    pub result: Option<String>,
    pub rules: Option<String>,
    pub date: Option<String>,
    pub comment: Option<String>,
}

impl SgfGame {
    /// Parses the first game from an SGF string.
    pub fn parse(input: &str) -> Result<Self, SgfError> {
        Self::parse_collection(input)?
            .into_iter()
            .next()
            .ok_or(SgfError::InvalidFormat)
    }

    /// Parses every game in an SGF collection.
    pub fn parse_collection(input: &str) -> Result<Vec<Self>, SgfError> {
        let mut parser = Parser {
            bytes: input.as_bytes(),
            pos: 0,
        };
        parser.parse_collection()
    }

    /// Reconstructs the Engine [`TimeControl`] from `TM` and `OT` properties.
    pub fn time_control(&self) -> TimeControl {
        TimeControl::from_sgf_fields(self.main_time_secs, self.overtime.as_deref())
    }

    /// Sets `TM` and `OT` from an Engine [`TimeControl`].
    pub fn set_time_control(&mut self, control: &TimeControl) {
        let (tm, ot) = control.to_sgf_fields();
        self.main_time_secs = tm;
        self.overtime = ot;
    }

    /// Replays setup stones and moves onto a fresh [`Game`].
    pub fn replay_game(&self) -> Result<Game, SgfError> {
        // Setup properties use SGF's top-down rows; Engine coordinates count
        // from the bottom.
        let to_engine = |(col, row): (u8, u16)| {
            Coord::new(col, self.board_size.saturating_sub(row as u8) as u16)
        };
        let setup: Vec<(Color, Coord)> = self
            .setup_black
            .iter()
            .map(|&point| (Color::Black, to_engine(point)))
            .chain(
                self.setup_white
                    .iter()
                    .map(|&point| (Color::White, to_engine(point))),
            )
            .filter(|(_, coord)| coord.is_on_board(self.board_size))
            .collect();
        let moves: Vec<(Color, Option<Coord>)> = self
            .moves
            .iter()
            .map(|mv| (mv.color, mv.to_coord(self.board_size)))
            .collect();
        Game::from_setup_and_moves(self.board_size, &setup, &moves)
            .map_err(|_| SgfError::IllegalMove)
    }

    /// Replays all moves onto a new board and returns it.
    ///
    /// Prefer [`SgfGame::replay_game`] when captures and ko matter.
    pub fn replay(&self) -> Board {
        self.replay_game()
            .map(|game| game.board().clone())
            .unwrap_or_else(|_| Board::new(self.board_size))
    }

    /// Serializes this game to SGF format.
    pub fn to_sgf(&self) -> String {
        let mut result = String::new();
        result.push_str("(;GM[1]FF[4]CA[UTF-8]AP[ogs-tui:");
        result.push_str(env!("CARGO_PKG_VERSION"));
        result.push(']');
        result.push_str(&format!("SZ[{}]", self.board_size));
        result.push_str(&format!("KM[{}]", format_float(self.komi)));
        if let Some(handicap) = self.handicap.filter(|&h| h > 0) {
            result.push_str(&format!("HA[{handicap}]"));
        }
        if let Some(rules) = &self.rules {
            result.push_str(&format!("RU[{}]", escape(rules)));
        }
        if let Some(game_name) = &self.game_name {
            result.push_str(&format!("GN[{}]", escape(game_name)));
        }
        if let Some(event) = &self.event {
            result.push_str(&format!("EV[{}]", escape(event)));
        }
        if let Some(round) = &self.round {
            result.push_str(&format!("RO[{}]", escape(round)));
        }
        if let Some(name) = &self.player_black {
            result.push_str(&format!("PB[{}]", escape(name)));
        }
        if let Some(rank) = &self.black_rank {
            result.push_str(&format!("BR[{}]", escape(rank)));
        }
        if let Some(name) = &self.player_white {
            result.push_str(&format!("PW[{}]", escape(name)));
        }
        if let Some(rank) = &self.white_rank {
            result.push_str(&format!("WR[{}]", escape(rank)));
        }
        if let Some(main_secs) = self.main_time_secs {
            result.push_str(&format!("TM[{main_secs}]"));
        }
        if let Some(overtime) = &self.overtime {
            result.push_str(&format!("OT[{}]", escape(overtime)));
        }
        if let Some(date) = &self.date {
            result.push_str(&format!("DT[{}]", escape(date)));
        }
        if let Some(result_text) = &self.result {
            result.push_str(&format!("RE[{}]", escape(result_text)));
        }
        if !self.setup_black.is_empty() {
            result.push_str("AB");
            for &(col, row) in &self.setup_black {
                result.push_str(&format!("[{}]", encode_point(col, row)));
            }
        }
        if !self.setup_white.is_empty() {
            result.push_str("AW");
            for &(col, row) in &self.setup_white {
                result.push_str(&format!("[{}]", encode_point(col, row)));
            }
        }
        if let Some(comment) = &self.comment {
            result.push_str(&format!("C[{}]", escape(comment)));
        }

        for mv in &self.moves {
            let color_char = match mv.color {
                Color::Black => 'B',
                Color::White => 'W',
            };
            match (mv.col, mv.row) {
                (Some(col), Some(row)) => {
                    result.push_str(&format!(";{}[{}]", color_char, encode_point(col, row)));
                }
                _ => {
                    result.push_str(&format!(";{}[]", color_char));
                }
            }
        }

        result.push(')');
        result
    }
}

/// Builds an exportable SGF from an Engine game.
pub fn from_game(
    game: &Game,
    komi: f64,
    black: Option<String>,
    white: Option<String>,
) -> SgfGame {
    let mut setup_black = Vec::new();
    let mut setup_white = Vec::new();
    for (color, coord) in game.setup_stones() {
        // Convert Engine's bottom-up row to SGF's top-down row.
        let point = (
            coord.col,
            game.size().saturating_sub(coord.row as u8) as u16,
        );
        match color {
            Color::Black => setup_black.push(point),
            Color::White => setup_white.push(point),
        }
    }
    SgfGame {
        board_size: game.size(),
        moves: game
            .history()
            .iter()
            .map(|mv| match mv.coord {
                Some(coord) => SgfMove::from_coord(mv.color, coord, game.size()),
                None => SgfMove {
                    color: mv.color,
                    col: None,
                    row: None,
                },
            })
            .collect(),
        setup_black,
        setup_white,
        komi,
        player_black: black,
        player_white: white,
        ..SgfGame::default()
    }
}

fn encode_point(col: u8, row: u16) -> String {
    let c = (b'a' + col.min(25)) as char;
    let r = (b'a' + row.min(25) as u8) as char;
    format!("{c}{r}")
}

fn decode_point(value: &str) -> Option<(u8, u16)> {
    let bytes = value.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let col = bytes[0].wrapping_sub(b'a');
    let row = bytes[1].wrapping_sub(b'a');
    if col >= 25 || row >= 25 {
        return None;
    }
    Some((col, row as u16))
}

fn format_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ']' => out.push_str("\\]"),
            _ => out.push(ch),
        }
    }
    out
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while let Some(byte) = self.bytes.get(self.pos) {
            if byte.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn parse_collection(&mut self) -> Result<Vec<SgfGame>, SgfError> {
        let mut games = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(b'(') => games.push(self.parse_tree(0)?),
                // Tolerate stray bytes before or between games.
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
        if games.is_empty() {
            return Err(SgfError::InvalidFormat);
        }
        Ok(games)
    }

    fn parse_tree(&mut self, depth: usize) -> Result<SgfGame, SgfError> {
        if depth > 64 {
            return Err(SgfError::NestingTooDeep);
        }
        if self.bump() != Some(b'(') {
            return Err(SgfError::InvalidFormat);
        }
        let mut game = SgfGame {
            board_size: 19,
            komi: 6.5,
            ..SgfGame::default()
        };
        let mut took_variation = false;

        loop {
            self.skip_ws();
            match self.peek() {
                Some(b';') => {
                    self.pos += 1;
                    let properties = self.parse_node()?;
                    game.apply_properties(&properties)?;
                }
                Some(b'(') => {
                    let child = self.parse_tree(depth + 1)?;
                    if !took_variation {
                        game.append_variation(child);
                        took_variation = true;
                    }
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                None => break,
                Some(_) => {
                    // Unknown token: skip it rather than rejecting the file.
                    self.pos += 1;
                }
            }
        }

        Ok(game)
    }

    fn parse_node(&mut self) -> Result<Vec<(String, Vec<String>)>, SgfError> {
        let mut properties = Vec::new();
        loop {
            self.skip_ws();
            let Some(first) = self.peek() else { break };
            if !first.is_ascii_alphabetic() {
                break;
            }
            let mut key = String::new();
            while let Some(byte) = self.peek() {
                if byte.is_ascii_alphabetic() {
                    key.push(byte.to_ascii_uppercase() as char);
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let mut values = Vec::new();
            loop {
                self.skip_ws();
                if self.peek() == Some(b'[') {
                    values.push(self.parse_value()?);
                } else {
                    break;
                }
            }
            if values.is_empty() {
                return Err(SgfError::MalformedProperty);
            }
            properties.push((key, values));
        }
        Ok(properties)
    }

    fn parse_value(&mut self) -> Result<String, SgfError> {
        if self.bump() != Some(b'[') {
            return Err(SgfError::MalformedProperty);
        }
        // Accumulate bytes rather than `char`s so UTF-8 text (CJK comments,
        // player names) survives the byte-oriented parser intact.
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.bump() {
                Some(b']') => break,
                Some(b'\\') => match self.bump() {
                    // An escaped line break is removed, per the FF[4] spec.
                    Some(b'\n') => {}
                    Some(b'\r') => {
                        if self.peek() == Some(b'\n') {
                            self.pos += 1;
                        }
                    }
                    Some(other) => out.push(other),
                    None => return Err(SgfError::UnterminatedValue),
                },
                Some(byte) => out.push(byte),
                None => return Err(SgfError::UnterminatedValue),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

impl SgfGame {
    fn apply_properties(
        &mut self,
        properties: &[(String, Vec<String>)],
    ) -> Result<(), SgfError> {
        for (key, values) in properties {
            let first = values.first().map(String::as_str).unwrap_or_default();
            match key.as_str() {
                "SZ" => {
                    let size = first
                        .split(':')
                        .next()
                        .and_then(|s| s.parse::<u8>().ok())
                        .ok_or(SgfError::InvalidBoardSize)?;
                    if !(2..=25).contains(&size) {
                        return Err(SgfError::InvalidBoardSize);
                    }
                    self.board_size = size;
                }
                "KM" => {
                    self.komi = first.parse::<f64>().map_err(|_| SgfError::InvalidKomi)?
                }
                "HA" => {
                    self.handicap = first.trim().parse::<u8>().ok();
                }
                "TM" => {
                    if let Ok(seconds) = first.trim().parse::<f64>() {
                        if seconds >= 0.0 {
                            self.main_time_secs = Some(seconds.round() as u32);
                        }
                    }
                }
                "OT" => self.overtime = Some(first.to_string()),
                "B" | "W" => {
                    let color = if key == "B" {
                        Color::Black
                    } else {
                        Color::White
                    };
                    let mv = match decode_point(first) {
                        Some((col, row)) => SgfMove {
                            color,
                            col: Some(col),
                            row: Some(row),
                        },
                        None => SgfMove {
                            color,
                            col: None,
                            row: None,
                        },
                    };
                    self.moves.push(mv);
                }
                "AB" | "AW" => {
                    for value in values {
                        if let Some((col, row)) = decode_point(value) {
                            if key == "AB" {
                                self.setup_black.push((col, row));
                            } else {
                                self.setup_white.push((col, row));
                            }
                        }
                    }
                }
                "PB" => self.player_black = Some(first.to_string()),
                "PW" => self.player_white = Some(first.to_string()),
                "BR" => self.black_rank = Some(first.to_string()),
                "WR" => self.white_rank = Some(first.to_string()),
                "GN" => self.game_name = Some(first.to_string()),
                "EV" => self.event = Some(first.to_string()),
                "RO" => self.round = Some(first.to_string()),
                "RE" => self.result = Some(first.to_string()),
                "RU" => self.rules = Some(first.to_string()),
                "DT" => self.date = Some(first.to_string()),
                "C" => self.comment = Some(first.to_string()),
                _ => {}
            }
        }
        Ok(())
    }

    fn append_variation(&mut self, child: SgfGame) {
        self.moves.extend(child.moves);
        self.setup_black.extend(child.setup_black);
        self.setup_white.extend(child.setup_white);
        if self.player_black.is_none() {
            self.player_black = child.player_black;
        }
        if self.player_white.is_none() {
            self.player_white = child.player_white;
        }
    }
}

/// Errors that can occur during SGF parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgfError {
    InvalidFormat,
    InvalidBoardSize,
    InvalidKomi,
    MalformedProperty,
    UnterminatedValue,
    NestingTooDeep,
    IllegalMove,
}

impl std::fmt::Display for SgfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFormat => write!(f, "invalid SGF format"),
            Self::InvalidBoardSize => write!(f, "invalid SGF board size"),
            Self::InvalidKomi => write!(f, "invalid SGF komi"),
            Self::MalformedProperty => write!(f, "malformed SGF property"),
            Self::UnterminatedValue => write!(f, "unterminated SGF property value"),
            Self::NestingTooDeep => write!(f, "SGF variations nested too deeply"),
            Self::IllegalMove => write!(f, "SGF contains an illegal move"),
        }
    }
}

impl std::error::Error for SgfError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parse_basic() {
        let sgf = "(;GM[1]FF[4]SZ[19];B[pd];W[dp])";
        let game = SgfGame::parse(sgf).unwrap();
        assert_eq!(game.board_size, 19);
        assert_eq!(game.moves.len(), 2);
        assert_eq!(game.moves[0].color, Color::Black);
        assert_eq!(game.moves[0].col, Some(15));
        // SGF rows are counted from the top: 'd' is row 3 from the top.
        assert_eq!(game.moves[0].row, Some(3));
        assert_eq!(game.moves[0].to_coord(19), Some(Coord::new(15, 16)));
    }

    #[test]
    fn parse_9x9() {
        let sgf = "(;GM[1]FF[4]SZ[9];B[cc];W[gg])";
        let game = SgfGame::parse(sgf).unwrap();
        assert_eq!(game.board_size, 9);
        assert_eq!(game.moves.len(), 2);
    }

    #[test]
    fn parse_pass() {
        let sgf = "(;GM[1]FF[4]SZ[19];B[];W[pd])";
        let game = SgfGame::parse(sgf).unwrap();
        assert_eq!(game.moves[0].col, None);
        assert_eq!(game.moves[0].row, None);
    }

    #[test]
    fn parse_komi_and_players() {
        let sgf = "(;GM[1]FF[4]SZ[9]KM[5.5]PB[Alice]PW[Bob]BR[3d]WR[4d]GN[Test Match]EV[Cup]RO[1]HA[2]TM[600]OT[5x30 byo-yomi]RU[chinese]RE[B+R]DT[2026-10-19];B[cc])";
        let game = SgfGame::parse(sgf).unwrap();
        assert_eq!(game.komi, 5.5);
        assert_eq!(game.player_black.as_deref(), Some("Alice"));
        assert_eq!(game.player_white.as_deref(), Some("Bob"));
        assert_eq!(game.black_rank.as_deref(), Some("3d"));
        assert_eq!(game.white_rank.as_deref(), Some("4d"));
        assert_eq!(game.game_name.as_deref(), Some("Test Match"));
        assert_eq!(game.event.as_deref(), Some("Cup"));
        assert_eq!(game.round.as_deref(), Some("1"));
        assert_eq!(game.handicap, Some(2));
        assert_eq!(game.main_time_secs, Some(600));
        assert_eq!(game.overtime.as_deref(), Some("5x30 byo-yomi"));
        assert_eq!(
            game.time_control(),
            TimeControl::Byoyomi {
                main: Duration::from_secs(600),
                periods: 5,
                period: Duration::from_secs(30),
            }
        );
        assert_eq!(game.rules.as_deref(), Some("chinese"));
        assert_eq!(game.result.as_deref(), Some("B+R"));

        let roundtrip = SgfGame::parse(&game.to_sgf()).unwrap();
        assert_eq!(roundtrip.black_rank, game.black_rank);
        assert_eq!(roundtrip.white_rank, game.white_rank);
        assert_eq!(roundtrip.game_name, game.game_name);
        assert_eq!(roundtrip.time_control(), game.time_control());
    }

    #[test]
    fn parse_escaped_comment() {
        let sgf = r"(;GM[1]FF[4]SZ[9]C[a \] b \\ c];B[cc])";
        let game = SgfGame::parse(sgf).unwrap();
        assert_eq!(game.comment.as_deref(), Some(r"a ] b \ c"));
        assert_eq!(game.moves.len(), 1);
    }

    #[test]
    fn parse_setup_stones() {
        let sgf = "(;GM[1]FF[4]SZ[9]AB[cc][dd]AW[ee];W[gg])";
        let game = SgfGame::parse(sgf).unwrap();
        assert_eq!(game.setup_black, vec![(2, 2), (3, 3)]);
        assert_eq!(game.setup_white, vec![(4, 4)]);
        let replay = game.replay_game().unwrap();
        assert_eq!(replay.board().get(2, 7), Some(Color::Black));
        assert_eq!(replay.board().get(4, 5), Some(Color::White));
        // White moved first (handicap), so Black is to play next.
        assert_eq!(replay.to_play(), Color::Black);
    }

    #[test]
    fn parse_uses_only_the_first_variation() {
        let sgf = "(;GM[1]FF[4]SZ[9];B[cc](;W[gg];B[cg])(;W[gc]))";
        let game = SgfGame::parse(sgf).unwrap();
        assert_eq!(game.moves.len(), 3);
        assert_eq!(game.moves[1].col, Some(6));
        assert_eq!(game.moves[2].col, Some(2));
    }

    #[test]
    fn parse_collection_of_two_games() {
        let sgf = "(;GM[1]FF[4]SZ[9];B[cc])(;GM[1]FF[4]SZ[9];B[gg])";
        let games = SgfGame::parse_collection(sgf).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].moves[0].col, Some(2));
        assert_eq!(games[1].moves[0].col, Some(6));
    }

    #[test]
    fn invalid_format() {
        assert_eq!(SgfGame::parse("not sgf"), Err(SgfError::InvalidFormat));
        assert_eq!(
            SgfGame::parse("(;GM[1]SZ[abc])"),
            Err(SgfError::InvalidBoardSize)
        );
    }

    #[test]
    fn roundtrip() {
        let original = "(;GM[1]FF[4]SZ[9]KM[6.5];B[cc];W[gg];B[gc];W[])";
        let game = SgfGame::parse(original).unwrap();
        let sgf_output = game.to_sgf();
        let reparsed = SgfGame::parse(&sgf_output).unwrap();
        assert_eq!(game.board_size, reparsed.board_size);
        assert_eq!(game.moves, reparsed.moves);
        assert_eq!(game.komi, reparsed.komi);
    }

    #[test]
    fn roundtrip_keeps_setup_stones() {
        let game = SgfGame {
            board_size: 13,
            setup_black: vec![(3, 3), (9, 9)],
            setup_white: vec![(6, 6)],
            moves: vec![SgfMove {
                color: Color::White,
                col: Some(2),
                row: Some(10),
            }],
            komi: 0.5,
            ..SgfGame::default()
        };
        let parsed = SgfGame::parse(&game.to_sgf()).unwrap();
        assert_eq!(parsed.setup_black, game.setup_black);
        assert_eq!(parsed.setup_white, game.setup_white);
        assert_eq!(parsed.moves, game.moves);
    }

    #[test]
    fn replay_basic() {
        let sgf = "(;GM[1]FF[4]SZ[9];B[cc];W[gg])";
        let game = SgfGame::parse(sgf).unwrap();
        let board = game.replay();
        // On 9x9, 'c' = col 2, 'g' = col 6; row 2 from the top is row 7 from
        // the bottom.
        assert_eq!(board.get(2, 7), Some(Color::Black));
        assert_eq!(board.get(6, 3), Some(Color::White));
    }

    #[test]
    fn export_from_engine_game() {
        let mut game = Game::new(9);
        game.play(Coord::new(2, 2)).unwrap();
        game.play(Coord::new(6, 6)).unwrap();
        let sgf = from_game(&game, 6.5, Some("A".into()), Some("B".into()));
        let text = sgf.to_sgf();
        assert!(text.contains(";B[ch]"), "unexpected export: {text}");
        assert!(text.contains(";W[gd]"), "unexpected export: {text}");
        let parsed = SgfGame::parse(&text).unwrap();
        assert_eq!(parsed.replay_game().unwrap().move_number(), 2);
    }

    #[test]
    fn illegal_replay_is_reported() {
        // Two black moves on the same point make the replay fail.
        let game = SgfGame {
            board_size: 9,
            moves: vec![
                SgfMove {
                    color: Color::Black,
                    col: Some(2),
                    row: Some(2),
                },
                SgfMove {
                    color: Color::White,
                    col: Some(2),
                    row: Some(2),
                },
            ],
            komi: 6.5,
            ..SgfGame::default()
        };
        assert!(matches!(
            game.replay_game(),
            Err(SgfError::IllegalMove)
        ));
    }
}
