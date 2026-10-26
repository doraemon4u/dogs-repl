//! Full game state machine built on top of [`Board`] and [`rules`].
//!
//! This is the pure-Engine counterpart of the server's game state: it owns the
//! board, whose turn it is, the ko point, captures, and the move history. It
//! never performs I/O; the Session layer feeds it moves from the Realtime API
//! and the SGF reader feeds it moves from disk.

use super::board::{Board, Color};
use super::coord::Coord;
use super::rules::{self, MoveError};
use std::collections::HashSet;

/// One move in the game history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayedMove {
    /// 1-based move number.
    pub number: u16,
    pub color: Color,
    /// `None` for a pass.
    pub coord: Option<Coord>,
    /// Stones captured by this move.
    pub captured: Vec<Coord>,
}

/// A game in progress.
#[derive(Debug, Clone)]
pub struct Game {
    board: Board,
    to_play: Color,
    initial_to_play: Color,
    ko_point: Option<(u8, u16)>,
    move_number: u16,
    consecutive_passes: u8,
    captured: [u16; 2],
    history: Vec<PlayedMove>,
    setup: Vec<(Color, Coord)>,
    prior_positions: HashSet<Vec<Option<Color>>>,
}

impl Game {
    /// Creates an empty game with Black to play.
    pub fn new(size: u8) -> Self {
        let board = Board::new(size);
        let mut prior_positions = HashSet::new();
        prior_positions.insert(board.grid_signature().to_vec());
        Self {
            board,
            to_play: Color::Black,
            initial_to_play: Color::Black,
            ko_point: None,
            move_number: 0,
            consecutive_passes: 0,
            captured: [0, 0],
            history: Vec::new(),
            setup: Vec::new(),
            prior_positions,
        }
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub const fn size(&self) -> u8 {
        self.board.size()
    }

    pub const fn to_play(&self) -> Color {
        self.to_play
    }

    pub const fn ko_point(&self) -> Option<(u8, u16)> {
        self.ko_point
    }

    pub fn prior_positions(&self) -> &HashSet<Vec<Option<Color>>> {
        &self.prior_positions
    }

    pub const fn move_number(&self) -> u16 {
        self.move_number
    }

    pub const fn consecutive_passes(&self) -> u8 {
        self.consecutive_passes
    }

    /// Number of stones this color has captured.
    pub const fn captures(&self, color: Color) -> u16 {
        match color {
            Color::Black => self.captured[0],
            Color::White => self.captured[1],
        }
    }

    /// The stones this color has lost (captured by the opponent).
    pub const fn lost(&self, color: Color) -> u16 {
        self.captures(color.opponent())
    }

    pub fn history(&self) -> &[PlayedMove] {
        &self.history
    }

    pub fn last_move(&self) -> Option<&PlayedMove> {
        self.history.last()
    }

    /// Stones placed before the first move (handicap or problem setup).
    pub fn setup_stones(&self) -> &[(Color, Coord)] {
        &self.setup
    }

    /// Places a setup stone without starting the game or capturing anything.
    pub fn place_setup(&mut self, color: Color, coord: Coord) -> Result<(), MoveError> {
        if !coord.is_on_board(self.size()) {
            return Err(MoveError::OffBoard);
        }
        if self.board.get(coord.col, coord.row).is_some() {
            return Err(MoveError::Occupied);
        }
        self.board.place(coord.col, coord.row, color);
        self.setup.push((color, coord));
        if self.history.is_empty() {
            self.prior_positions.clear();
            self.prior_positions
                .insert(self.board.grid_signature().to_vec());
        }
        Ok(())
    }

    /// Overrides whose turn it is (handicap games start with White).
    pub fn set_to_play(&mut self, color: Color) {
        if self.history.is_empty() {
            self.initial_to_play = color;
        }
        self.to_play = color;
    }

    /// Two consecutive passes end the play phase.
    pub const fn is_finished(&self) -> bool {
        self.consecutive_passes >= 2
    }

    /// Validates whether `coord` is legal for `self.to_play()` without mutating the game.
    pub fn validate(&self, coord: Coord) -> Result<usize, MoveError> {
        if !coord.is_on_board(self.size()) {
            return Err(MoveError::OffBoard);
        }
        rules::validate_move_with_superko(
            self.board(),
            coord.col,
            coord.row,
            self.to_play,
            self.ko_point,
            &self.prior_positions,
        )
    }

    /// Plays a move for `to_play`.
    pub fn play(&mut self, coord: Coord) -> Result<&PlayedMove, MoveError> {
        if !coord.is_on_board(self.size()) {
            return Err(MoveError::OffBoard);
        }
        let color = self.to_play;
        rules::validate_move_with_superko(
            self.board(),
            coord.col,
            coord.row,
            color,
            self.ko_point,
            &self.prior_positions,
        )?;
        let captured = rules::execute_move(&mut self.board, coord.col, coord.row, color);
        self.record_captures(color, &captured);
        self.ko_point = detect_ko(&self.board, coord.col, coord.row, &captured);
        self.prior_positions
            .insert(self.board.grid_signature().to_vec());
        self.push_move(color, Some(coord), captured);
        Ok(self.history.last().expect("move was just pushed"))
    }

    /// Plays a move for a specific color, used when replaying SGF or server
    /// history where handicap games do not strictly alternate from Black.
    pub fn play_colored(
        &mut self,
        color: Color,
        coord: Coord,
    ) -> Result<&PlayedMove, MoveError> {
        if !coord.is_on_board(self.size()) {
            return Err(MoveError::OffBoard);
        }
        rules::validate_move_with_superko(
            self.board(),
            coord.col,
            coord.row,
            color,
            self.ko_point,
            &self.prior_positions,
        )?;
        let captured = rules::execute_move(&mut self.board, coord.col, coord.row, color);
        self.record_captures(color, &captured);
        self.ko_point = detect_ko(&self.board, coord.col, coord.row, &captured);
        self.prior_positions
            .insert(self.board.grid_signature().to_vec());
        self.push_move(color, Some(coord), captured);
        Ok(self.history.last().expect("move was just pushed"))
    }

    /// Passes for `to_play`.
    pub fn pass(&mut self) -> &PlayedMove {
        self.ko_point = None;
        let color = self.to_play;
        self.push_move(color, None, Vec::new());
        self.history.last().expect("pass was just pushed")
    }

    /// Removes the last move and rebuilds the position from history.
    pub fn undo(&mut self) -> Option<PlayedMove> {
        if self.history.is_empty() {
            return None;
        }
        let mut prior = self.history.clone();
        let removed = prior.pop()?;
        let size = self.size();
        let setup = self.setup.clone();
        let initial_to_play = self.initial_to_play;
        *self = Self::new(size);
        self.initial_to_play = initial_to_play;
        for (color, coord) in setup {
            let _ = self.place_setup(color, coord);
        }
        self.to_play = prior
            .first()
            .map(|mv| mv.color)
            .unwrap_or(initial_to_play);
        for mv in prior {
            match mv.coord {
                Some(coord) => {
                    let _ = self.play_colored(mv.color, coord);
                }
                None => {
                    self.to_play = mv.color;
                    self.pass();
                }
            }
        }
        Some(removed)
    }

    /// Replays a list of moves from an empty board.
    pub fn from_moves(
        size: u8,
        moves: &[(Color, Option<Coord>)],
    ) -> Result<Self, MoveError> {
        Self::from_setup_and_moves(size, &[], moves)
    }

    /// Replays setup stones followed by moves.
    pub fn from_setup_and_moves(
        size: u8,
        setup: &[(Color, Coord)],
        moves: &[(Color, Option<Coord>)],
    ) -> Result<Self, MoveError> {
        let mut game = Self::new(size);
        for (color, coord) in setup {
            game.place_setup(*color, *coord)?;
        }
        if let Some((color, _)) = moves.first() {
            game.set_to_play(*color);
        }
        for (color, coord) in moves {
            match coord {
                Some(coord) => {
                    game.play_colored(*color, *coord)?;
                }
                None => {
                    game.to_play = *color;
                    game.pass();
                }
            }
        }
        Ok(game)
    }

    /// Replays the moves that precede `move_number`, for SGF server sync.
    pub fn truncate_to(&mut self, move_number: u16) -> Result<(), MoveError> {
        if move_number as usize >= self.history.len() {
            return Ok(());
        }
        let moves: Vec<(Color, Option<Coord>)> = self
            .history
            .iter()
            .take(move_number as usize)
            .map(|mv| (mv.color, mv.coord))
            .collect();
        let setup = self.setup.clone();
        let initial_to_play = self.initial_to_play;
        *self = Self::new(self.size());
        self.initial_to_play = initial_to_play;
        for (color, coord) in setup {
            self.place_setup(color, coord)?;
        }
        self.to_play = moves
            .first()
            .map(|(c, _)| *c)
            .unwrap_or(initial_to_play);
        for (color, coord) in moves {
            match coord {
                Some(coord) => {
                    self.play_colored(color, coord)?;
                }
                None => {
                    self.to_play = color;
                    self.pass();
                }
            }
        }
        Ok(())
    }

    /// Returns a non-destructive snapshot of the game at `move_number`.
    pub fn game_at(&self, move_number: u16) -> Self {
        let mut copy = self.clone();
        if (move_number as usize) < copy.history.len() {
            let _ = copy.truncate_to(move_number);
        }
        copy
    }

    /// Returns the board position after `move_number` moves.
    pub fn board_at(&self, move_number: u16) -> Board {
        self.game_at(move_number).board().clone()
    }

    /// Returns `(black_captures, white_captures)` after `move_number` moves.
    pub fn captures_at(&self, move_number: u16) -> (u16, u16) {
        let snap = self.game_at(move_number);
        (snap.captures(Color::Black), snap.captures(Color::White))
    }

    fn record_captures(&mut self, color: Color, captured: &[(u8, u16)]) {
        let count = captured.len() as u16;
        match color {
            Color::Black => self.captured[0] = self.captured[0].saturating_add(count),
            Color::White => self.captured[1] = self.captured[1].saturating_add(count),
        }
    }

    fn push_move(
        &mut self,
        color: Color,
        coord: Option<Coord>,
        captured: Vec<(u8, u16)>,
    ) {
        self.move_number = self.move_number.saturating_add(1);
        if coord.is_none() {
            self.consecutive_passes = self.consecutive_passes.saturating_add(1);
        } else {
            self.consecutive_passes = 0;
        }
        self.history.push(PlayedMove {
            number: self.move_number,
            color,
            coord,
            captured: captured
                .into_iter()
                .map(|(c, r)| Coord::new(c, r))
                .collect(),
        });
        self.to_play = self.to_play.opponent();
    }
}

/// Simple-ko detection: a move creates a ko when it captures exactly one stone
/// and the capturing group is a single stone with exactly one liberty.
fn detect_ko(
    board: &Board,
    col: u8,
    row: u16,
    captured: &[(u8, u16)],
) -> Option<(u8, u16)> {
    if captured.len() != 1 {
        return None;
    }
    let group = rules::collect_group(board, col, row);
    if group.len() == 1 && rules::count_liberties(board, &group) == 1 {
        Some(captured[0])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(col: u8, row: u16) -> Coord {
        Coord::new(col, row)
    }

    #[test]
    fn alternates_turns_and_tracks_history() {
        let mut game = Game::new(9);
        assert_eq!(game.to_play(), Color::Black);
        game.play(c(2, 2)).unwrap();
        assert_eq!(game.to_play(), Color::White);
        game.play(c(6, 6)).unwrap();
        assert_eq!(game.move_number(), 2);
        assert_eq!(game.history()[0].color, Color::Black);
        assert_eq!(game.history()[1].coord, Some(c(6, 6)));
    }

    #[test]
    fn rejects_occupied_and_off_board() {
        let mut game = Game::new(9);
        game.play(c(2, 2)).unwrap();
        assert_eq!(game.play(c(2, 2)), Err(MoveError::Occupied));
        assert_eq!(game.play(c(9, 2)), Err(MoveError::OffBoard));
        assert_eq!(game.play(c(2, 10)), Err(MoveError::OffBoard));
    }

    #[test]
    fn captures_and_counts() {
        let mut game = Game::new(9);
        // White is boxed in by black at the corner-ish point (1,1).
        game.play_colored(Color::White, c(1, 1)).unwrap();
        game.play_colored(Color::Black, c(0, 1)).unwrap();
        game.play_colored(Color::Black, c(2, 1)).unwrap();
        game.play_colored(Color::Black, c(1, 2)).unwrap();
        // Actually playing Black three times in a row is only used to build the
        // shape; the final capture below is the assertion under test.
        game.play_colored(Color::Black, c(1, 3)).unwrap();
        // (1,1) had liberties (1,3) only after the wall, and it is now filled.
        assert_eq!(game.board().get(1, 1), None);
        assert_eq!(game.captures(Color::Black), 1);
        assert_eq!(game.lost(Color::White), 1);
    }

    #[test]
    fn simple_ko_is_detected_and_enforced() {
        // Build the ko shape as setup stones so the pre-set groups are not
        // captured while the position is being assembled.
        let setup = [
            (Color::Black, c(0, 3)),
            (Color::White, c(1, 3)),
            (Color::Black, c(2, 3)),
            (Color::White, c(0, 2)),
            (Color::Black, c(1, 2)),
            (Color::Black, c(3, 2)),
            (Color::Black, c(4, 2)),
            (Color::Black, c(0, 1)),
            (Color::White, c(1, 1)),
            (Color::Black, c(2, 1)),
        ];
        let mut game = Game::from_setup_and_moves(9, &setup, &[]).unwrap();
        game.set_to_play(Color::White);
        game.play(c(2, 2)).unwrap();
        assert_eq!(game.board().get(1, 2), None);
        assert_eq!(game.ko_point(), Some((1, 2)));
        assert_eq!(game.play(c(1, 2)), Err(MoveError::Ko));
        // After one move elsewhere the ko is legal again.
        game.play(c(6, 6)).unwrap();
        game.play(c(6, 7)).unwrap();
        game.play(c(1, 2)).unwrap();
        assert_eq!(game.board().get(2, 2), None);
    }

    #[test]
    fn passes_finish_the_play_phase() {
        let mut game = Game::new(9);
        game.play(c(2, 2)).unwrap();
        game.pass();
        assert!(!game.is_finished());
        game.pass();
        assert!(game.is_finished());
        assert_eq!(game.consecutive_passes(), 2);
    }

    #[test]
    fn undo_restores_the_previous_position() {
        let mut game = Game::new(9);
        game.play(c(2, 2)).unwrap();
        game.play(c(6, 6)).unwrap();
        assert_eq!(game.move_number(), 2);
        let removed = game.undo().unwrap();
        assert_eq!(removed.coord, Some(c(6, 6)));
        assert_eq!(game.move_number(), 1);
        assert_eq!(game.to_play(), Color::White);
        assert_eq!(game.board().get(6, 6), None);
        assert_eq!(game.board().get(2, 2), Some(Color::Black));
    }

    #[test]
    fn undo_restores_captured_stones() {
        // A capture followed by undoing the capturing move must put the
        // captured stone back. Undoing only the follow-up move must not.
        let mut game = Game::new(9);
        for (color, coord) in [
            (Color::White, c(1, 1)),
            (Color::Black, c(0, 1)),
            (Color::Black, c(2, 1)),
            (Color::Black, c(1, 2)),
            (Color::Black, c(1, 3)),
        ] {
            game.play_colored(color, coord).unwrap();
        }
        assert_eq!(game.board().get(1, 1), None);
        game.undo();
        assert_eq!(game.board().get(1, 1), None);
        game.undo();
        assert_eq!(game.board().get(1, 1), Some(Color::White));
    }

    #[test]
    fn from_moves_replays_sgf_style_history() {
        let moves = [
            (Color::Black, Some(c(2, 2))),
            (Color::White, Some(c(6, 6))),
            (Color::White, None),
        ];
        let game = Game::from_moves(9, &moves).unwrap();
        assert_eq!(game.move_number(), 3);
        assert_eq!(game.board().get(2, 2), Some(Color::Black));
    }

    #[test]
    fn truncate_to_rebuilds_earlier_position() {
        let mut game = Game::new(9);
        game.play(c(2, 2)).unwrap();
        game.play(c(6, 6)).unwrap();
        game.play(c(3, 3)).unwrap();
        game.truncate_to(2).unwrap();
        assert_eq!(game.move_number(), 2);
        assert_eq!(game.board().get(3, 3), None);
    }

    #[test]
    fn game_at_is_non_destructive() {
        let mut game = Game::new(9);
        game.play(c(2, 2)).unwrap();
        game.play(c(6, 6)).unwrap();
        game.play(c(3, 3)).unwrap();
        let snap = game.game_at(1);
        assert_eq!(snap.move_number(), 1);
        assert_eq!(snap.board().get(2, 2), Some(Color::Black));
        assert_eq!(snap.board().get(6, 6), None);
        // Original game still has all 3 moves.
        assert_eq!(game.move_number(), 3);
        assert_eq!(game.board().get(3, 3), Some(Color::Black));
    }

    #[test]
    fn undo_and_truncate_to_zero_preserve_white_initial_turn() {
        let mut game = Game::new(9);
        game.set_to_play(Color::White);
        assert_eq!(game.to_play(), Color::White);
        game.play(c(2, 2)).unwrap();
        assert_eq!(game.to_play(), Color::Black);
        game.undo();
        assert_eq!(game.to_play(), Color::White);
        assert_eq!(game.move_number(), 0);

        game.play(c(2, 2)).unwrap();
        game.play(c(6, 6)).unwrap();
        game.truncate_to(0).unwrap();
        assert_eq!(game.to_play(), Color::White);
        assert_eq!(game.move_number(), 0);
    }

    #[test]
    fn dead_eye_suicide_and_superko_are_rejected() {
        let setup = [
            (Color::Black, c(0, 3)),
            (Color::White, c(1, 3)),
            (Color::Black, c(2, 3)),
            (Color::White, c(0, 2)),
            (Color::Black, c(1, 2)),
            (Color::Black, c(3, 2)),
            (Color::Black, c(4, 2)),
            (Color::Black, c(0, 1)),
            (Color::White, c(1, 1)),
            (Color::Black, c(2, 1)),
            // Dead eye for White at (8,1) surrounded by Black at (7,1) and (8,2)
            (Color::Black, c(7, 1)),
            (Color::Black, c(8, 2)),
        ];
        let mut game = Game::from_setup_and_moves(9, &setup, &[]).unwrap();
        game.set_to_play(Color::White);
        // White cannot play into the dead eye at (8,1)
        assert_eq!(game.play(c(8, 1)), Err(MoveError::Suicide));

        // White captures at (2,2), creating ko at (1,2)
        game.play(c(2, 2)).unwrap();
        // Immediate recapture at (1,2) is rejected by Ko
        assert_eq!(game.play(c(1, 2)), Err(MoveError::Ko));
        // If both players pass (clearing simple ko without changing the board),
        // recapturing at (1,2) would recreate the initial setup board position and is rejected by Superko!
        game.pass();
        game.pass();
        assert_eq!(game.play(c(1, 2)), Err(MoveError::Superko));
    }
}
