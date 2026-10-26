//! Go rule validation: capture, ko, and suicide prevention.
//!
//! Operates on [`Board`](crate::engine::board::Board) to validate moves.

use super::board::{Board, Color};

/// Result of a move validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveError {
    /// Position is already occupied.
    Occupied,
    /// Move would capture own stones (suicide).
    Suicide,
    /// Move violates the simple ko rule.
    Ko,
    /// Coordinate is outside the board.
    OffBoard,
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Occupied => write!(f, "position already occupied"),
            Self::Suicide => write!(f, "suicide move"),
            Self::Ko => write!(f, "ko rule violation"),
            Self::OffBoard => write!(f, "coordinate is outside the board"),
        }
    }
}

impl std::error::Error for MoveError {}

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
    if col >= board.size() || row == 0 || row > board.size() as u16 {
        return Err(MoveError::OffBoard);
    }

    if board.get(col, row).is_some() {
        return Err(MoveError::Occupied);
    }

    if ko_point == Some((col, row)) {
        return Err(MoveError::Ko);
    }

    // Simulate the move on a lightweight basis: check captures first
    let opponent = color.opponent();
    let mut captured = 0;
    let mut counted_stones = std::collections::HashSet::new();

    for (nc, nr) in board.neighbors(col, row) {
        if board.get(nc, nr) == Some(opponent) && !counted_stones.contains(&(nc, nr)) {
            let group = collect_group(board, nc, nr);
            if count_liberties_excluding(board, &group, (col, row)) == 0 {
                captured += group.len();
                for pt in &group {
                    counted_stones.insert(*pt);
                }
            }
        }
    }

    // Check suicide: if no captures, the placed stone must have liberties
    if captured == 0 {
        let group = collect_group_with_move(board, col, row, color);
        if count_liberties_with_move(board, &group, col, row) == 0 {
            return Err(MoveError::Suicide);
        }
    }

    Ok(captured)
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

/// Collects the group that would contain (col, row) if `color` were placed there.
fn collect_group_with_move(board: &Board, col: u8, row: u16, color: Color) -> Vec<(u8, u16)> {
    let mut visited = vec![(col, row)];
    let mut stack: Vec<(u8, u16)> = board
        .neighbors(col, row)
        .into_iter()
        .filter(|&(c, r)| board.get(c, r) == Some(color))
        .collect();

    while let Some((c, r)) = stack.pop() {
        if visited.contains(&(c, r)) {
            continue;
        }
        visited.push((c, r));
        for (nc, nr) in board.neighbors(c, r) {
            if board.get(nc, nr) == Some(color) && !visited.contains(&(nc, nr)) {
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

/// Counts liberties of a group, excluding a specific position (treated as occupied).
fn count_liberties_excluding(board: &Board, group: &[(u8, u16)], exclude: (u8, u16)) -> usize {
    let mut liberties = Vec::new();
    for &(c, r) in group {
        for (nc, nr) in board.neighbors(c, r) {
            if (nc, nr) == exclude {
                continue;
            }
            if board.is_empty(nc, nr) && !liberties.contains(&(nc, nr)) {
                liberties.push((nc, nr));
            }
        }
    }
    liberties.len()
}

/// Counts liberties of a group that includes a hypothetical stone at (move_col, move_row).
fn count_liberties_with_move(
    board: &Board,
    group: &[(u8, u16)],
    move_col: u8,
    move_row: u16,
) -> usize {
    let mut liberties = Vec::new();
    for &(c, r) in group {
        for (nc, nr) in board.neighbors(c, r) {
            // The move point itself is occupied by the new stone
            if (nc, nr) == (move_col, move_row) {
                continue;
            }
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
        // Surround a white stone at (1,1) with black
        b.place(1, 1, Color::White);
        b.place(0, 1, Color::Black);
        b.place(2, 1, Color::Black);
        b.place(1, 2, Color::Black);
        // Last liberty at (1,0) doesn't exist (row 0 is invalid), so (1,1) has liberty at... let me redo
        // Actually on 9x9, (1,1) neighbors are (0,1), (2,1), (1,2). Row 0 is invalid.
        // So white at (1,1) has 3 neighbors. Place black on all 3.
        let result = validate_move(&b, 1, 1, Color::Black, None);
        // Can't place on occupied
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
    fn suicide_prevention() {
        let mut b = Board::new(9);
        // Create a position where white has no liberties
        b.place(0, 1, Color::Black);
        b.place(2, 1, Color::Black);
        b.place(1, 2, Color::Black);
        // (1,1) is surrounded by black on 3 sides, row 0 doesn't exist
        // So (1,1) has no liberties for white
        assert_eq!(validate_move(&b, 1, 1, Color::White, None), Err(MoveError::Suicide));
    }

    #[test]
    fn ko_rule() {
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

        // White plays at (2,2), capturing the black stone at (1,2)
        assert_eq!(validate_move(&b, 2, 2, Color::White, None), Ok(1));
        let captured = execute_move(&mut b, 2, 2, Color::White);
        assert_eq!(captured.len(), 1);
        assert!(captured.contains(&(1, 2)));

        // Black cannot immediately recapture at (1,2) — ko point
        let ko_point = Some((1, 2));
        assert_eq!(validate_move(&b, 1, 2, Color::Black, ko_point), Err(MoveError::Ko));
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
        let mut b = Board::new(9);
        // Place a 2-stone white group at (1,1) and (1,2)
        b.place(1, 1, Color::White);
        b.place(1, 2, Color::White);
        // Surround with black stones except at (0,1)
        b.place(2, 1, Color::Black);
        b.place(2, 2, Color::Black);
        b.place(1, 3, Color::Black);
        b.place(0, 2, Color::Black);
        // Now (1,1) and (1,2) have their only remaining liberty at (0,1).
        // (0,1) is adjacent to (1,1).
        // Let's create an L shape: White at (0,2) and (1,1) and played move at (0,1).
        // (0,1) neighbors are (0,2) and (1,1).
        // If White has stones at both (0,2) and (1,1) connected via (1,2):
        // White stones: (1,1), (1,2), (0,2).
        // When Black plays at (0,1), both (0,2) and (1,1) are neighbors of (0,1) and belong to the same White group.
        let mut b2 = Board::new(9);
        b2.place(1, 1, Color::White);
        b2.place(1, 2, Color::White);
        b2.place(0, 2, Color::White);
        // Outside Black surround:
        b2.place(2, 1, Color::Black);
        b2.place(2, 2, Color::Black);
        b2.place(1, 3, Color::Black);
        b2.place(0, 3, Color::Black);
        // (0,1) is the last liberty for the White group!
        // (0,1)'s neighbors are (0,2) [White] and (1,1) [White].
        // Both are in the same White group of size 3!
        let result = validate_move(&b2, 0, 1, Color::Black, None);
        assert_eq!(result, Ok(3));
        let captured = execute_move(&mut b2, 0, 1, Color::Black);
        assert_eq!(captured.len(), 3);
    }
}
