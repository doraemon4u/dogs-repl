//! Go rule validation: capture, ko, and suicide prevention.
//!
//! Operates on [`Board`](crate::engine::board::Board) to validate moves.

use super::board::{Board, Color};

/// Result of a move validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveError {
    /// Position is already occupied.
    Occupied,
    /// Move would result in 0 liberties for own group without capturing opponent stones (suicide).
    Suicide,
    /// Move violates the simple ko rule (immediate recapture of single ko stone).
    Ko,
    /// Move repeats a previous full-board position (positional superko).
    Superko,
    /// Coordinate is outside the board.
    OffBoard,
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Occupied => write!(f, "position already occupied (已有棋子，不可落子)"),
            Self::Suicide => write!(f, "suicide move into dead eye (禁入点：死眼无气自杀禁手)"),
            Self::Ko => write!(f, "ko rule violation (打劫禁手：刚提之眼不可立即回提)"),
            Self::Superko => write!(f, "superko rule violation (全局同形禁手：盘面不可重复)"),
            Self::OffBoard => write!(f, "coordinate is outside the board (坐标超出棋盘范围)"),
        }
    }
}

impl std::error::Error for MoveError {}

/// Simulates placing `color` at `(col, row)`, removing captured opponent groups
/// first, then verifying that the resulting friendly group has at least one liberty.
///
/// Returns `Ok((next_board, captured_positions))` if legal under basic Go rules
/// (bounds, empty point, simple ko, and non-suicide).
pub fn simulate_move(
    board: &Board,
    col: u8,
    row: u16,
    color: Color,
    ko_point: Option<(u8, u16)>,
) -> Result<(Board, Vec<(u8, u16)>), MoveError> {
    if col >= board.size() || row == 0 || row > board.size() as u16 {
        return Err(MoveError::OffBoard);
    }

    if board.get(col, row).is_some() {
        return Err(MoveError::Occupied);
    }

    if ko_point == Some((col, row)) {
        return Err(MoveError::Ko);
    }

    let mut next_board = board.clone();
    let captured = execute_move(&mut next_board, col, row, color);

    // After opponent 0-liberty groups are removed, check our own connected group's liberties.
    let own_group = collect_group(&next_board, col, row);
    if count_liberties(&next_board, &own_group) == 0 {
        return Err(MoveError::Suicide);
    }

    Ok((next_board, captured))
}

/// Validates a move without modifying the board.
///
/// Returns `Ok(captured_stones)` if the move is valid, where `captured_stones`
/// is the number of opponent stones that would be captured.
/// Returns [`MoveError`] if the move is illegal.
pub fn validate_move(
    board: &Board,
    col: u8,
    row: u16,
    color: Color,
    ko_point: Option<(u8, u16)>,
) -> Result<usize, MoveError> {
    let (_next_board, captured) = simulate_move(board, col, row, color, ko_point)?;
    Ok(captured.len())
}

/// Validates a move including positional superko (whole-board repetition) check.
pub fn validate_move_with_superko(
    board: &Board,
    col: u8,
    row: u16,
    color: Color,
    ko_point: Option<(u8, u16)>,
    prior_positions: &std::collections::HashSet<Vec<Option<Color>>>,
) -> Result<usize, MoveError> {
    let (next_board, captured) = simulate_move(board, col, row, color, ko_point)?;
    if prior_positions.contains(next_board.grid_signature()) {
        return Err(MoveError::Superko);
    }
    Ok(captured.len())
}

/// Collects all stones in the connected group containing (col, row).
pub fn collect_group(board: &Board, col: u8, row: u16) -> Vec<(u8, u16)> {
    let Some(color) = board.get(col, row) else {
        return Vec::new();
    };

    let mut visited = Vec::new();
    let mut stack = vec![(col, row)];

    while let Some((c, r)) = stack.pop() {
        if visited.contains(&(c, r)) {
            continue;
        }
        if board.get(c, r) != Some(color) {
            continue;
        }
        visited.push((c, r));
        for (nc, nr) in board.neighbors(c, r) {
            if !visited.contains(&(nc, nr)) {
                stack.push((nc, nr));
            }
        }
    }

    visited
}

/// Counts liberties (empty adjacent points) of a group.
pub fn count_liberties(board: &Board, group: &[(u8, u16)]) -> usize {
    let mut liberties = Vec::new();
    for &(c, r) in group {
        for (nc, nr) in board.neighbors(c, r) {
            if board.is_empty(nc, nr) && !liberties.contains(&(nc, nr)) {
                liberties.push((nc, nr));
            }
        }
    }
    liberties.len()
}

/// Executes a validated move on the board, removing captured stones.
/// Returns the positions of captured stones.
pub fn execute_move(
    board: &mut Board,
    col: u8,
    row: u16,
    color: Color,
) -> Vec<(u8, u16)> {
    board.place(col, row, color);

    let opponent = color.opponent();
    let mut captured = Vec::new();

    for (nc, nr) in board.neighbors(col, row) {
        if board.get(nc, nr) == Some(opponent) {
            let group = collect_group(board, nc, nr);
            if count_liberties(board, &group) == 0 {
                for &(c, r) in &group {
                    board.remove(c, r);
                    captured.push((c, r));
                }
            }
        }
    }

    captured
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn place_on_empty() {
        let mut b = Board::new(9);
        let result = validate_move(&b, 4, 4, Color::Black, None);
        assert_eq!(result, Ok(0));
        execute_move(&mut b, 4, 4, Color::Black);
        assert_eq!(b.get(4, 4), Some(Color::Black));
    }

    #[test]
    fn occupied_position() {
        let mut b = Board::new(9);
        b.place(4, 4, Color::Black);
        assert_eq!(validate_move(&b, 4, 4, Color::White, None), Err(MoveError::Occupied));
    }

    #[test]
    fn capture_single_stone() {
        let mut b = Board::new(9);
        b.place(1, 1, Color::White);
        b.place(0, 1, Color::Black);
        b.place(2, 1, Color::Black);
        b.place(1, 2, Color::Black);
        let result = validate_move(&b, 1, 1, Color::Black, None);
        assert_eq!(result, Err(MoveError::Occupied));
    }

    #[test]
    fn capture_scenario() {
        let mut b = Board::new(9);
        // White stone at (4,4), black surrounds on 3 sides
        b.place(4, 4, Color::White);
        b.place(3, 4, Color::Black);
        b.place(5, 4, Color::Black);
        b.place(4, 3, Color::Black);
        // White has 1 liberty at (4,5)
        let result = validate_move(&b, 4, 5, Color::Black, None);
        assert_eq!(result, Ok(1)); // Would capture 1 stone
        let captured = execute_move(&mut b, 4, 5, Color::Black);
        assert_eq!(captured.len(), 1);
        assert!(captured.contains(&(4, 4)));
        assert_eq!(b.get(4, 4), None);
    }

    #[test]
    fn suicide_prevention_single_and_group_dead_eye() {
        let mut b = Board::new(9);
        // Single-point dead eye at (1,1) surrounded by Black at (0,1), (2,1), (1,2)
        b.place(0, 1, Color::Black);
        b.place(2, 1, Color::Black);
        b.place(1, 2, Color::Black);
        assert_eq!(validate_move(&b, 1, 1, Color::White, None), Err(MoveError::Suicide));

        // Multi-stone group self-atari filling its own last eye:
        // White has a stone at (0,1), surrounded by Black at (0,2), (1,2), (2,1).
        // If White plays at (1,1), the 2-stone White group {(0,1),(1,1)} has 0 liberties and captures nothing.
        let mut b2 = Board::new(9);
        b2.place(0, 1, Color::White);
        b2.place(0, 2, Color::Black);
        b2.place(1, 2, Color::Black);
        b2.place(2, 1, Color::Black);
        assert_eq!(validate_move(&b2, 1, 1, Color::White, None), Err(MoveError::Suicide));
        // Meanwhile Black playing into (1,1) captures the White stone at (0,1), so it is legal!
        assert_eq!(validate_move(&b2, 1, 1, Color::Black, None), Ok(1));
    }

    #[test]
    fn ko_rule_and_positional_superko() {
        let mut b = Board::new(9);

        // Ko shape:
        // Row 3: B W B . .
        // Row 2: W B . B B
        // Row 1: B W B . .
        b.place(0, 3, Color::Black);
        b.place(1, 3, Color::White);
        b.place(2, 3, Color::Black);
        b.place(0, 2, Color::White);
        b.place(1, 2, Color::Black);
        b.place(3, 2, Color::Black);
        b.place(4, 2, Color::Black);
        b.place(0, 1, Color::Black);
        b.place(1, 1, Color::White);
        b.place(2, 1, Color::Black);

        let mut prior = HashSet::new();
        prior.insert(b.grid_signature().to_vec());

        // White plays at (2,2), capturing the black stone at (1,2)
        assert_eq!(validate_move_with_superko(&b, 2, 2, Color::White, None, &prior), Ok(1));
        let captured = execute_move(&mut b, 2, 2, Color::White);
        assert_eq!(captured.len(), 1);
        assert!(captured.contains(&(1, 2)));
        prior.insert(b.grid_signature().to_vec());

        // Black cannot immediately recapture at (1,2) — ko point
        let ko_point = Some((1, 2));
        assert_eq!(validate_move(&b, 1, 2, Color::Black, ko_point), Err(MoveError::Ko));

        // Even if ko_point were cleared (e.g. after passes without board change), positional superko blocks repeating `prior`!
        assert_eq!(
            validate_move_with_superko(&b, 1, 2, Color::Black, None, &prior),
            Err(MoveError::Superko)
        );
    }

    #[test]
    fn collect_group_basic() {
        let mut b = Board::new(9);
        b.place(3, 3, Color::Black);
        b.place(3, 4, Color::Black);
        b.place(4, 3, Color::Black);
        let group = collect_group(&b, 3, 3);
        assert_eq!(group.len(), 3);
        assert!(group.contains(&(3, 3)));
        assert!(group.contains(&(3, 4)));
        assert!(group.contains(&(4, 3)));
    }

    #[test]
    fn off_board_move() {
        let b = Board::new(9);
        assert_eq!(validate_move(&b, 9, 1, Color::Black, None), Err(MoveError::OffBoard));
        assert_eq!(validate_move(&b, 0, 0, Color::Black, None), Err(MoveError::OffBoard));
        assert_eq!(validate_move(&b, 0, 10, Color::Black, None), Err(MoveError::OffBoard));
    }

    #[test]
    fn capture_multi_stone_group_no_double_count() {
        let mut b2 = Board::new(9);
        b2.place(1, 1, Color::White);
        b2.place(1, 2, Color::White);
        b2.place(0, 2, Color::White);
        b2.place(2, 1, Color::Black);
        b2.place(2, 2, Color::Black);
        b2.place(1, 3, Color::Black);
        b2.place(0, 3, Color::Black);
        let result = validate_move(&b2, 0, 1, Color::Black, None);
        assert_eq!(result, Ok(3));
        let captured = execute_move(&mut b2, 0, 1, Color::Black);
        assert_eq!(captured.len(), 3);
    }
}
