//! Go rule validation: capture, ko, and suicide prevention.
//!
//! Operates on [`Board`](crate::engine::board::Board) to validate moves.
//!
//! Placement rules are identical for every board size: size only sets the
//! bounds. What differs between games is the [`RuleSet`].

use super::board::{Board, Color};

/// Board sizes a local game can be created with.
pub const SUPPORTED_SIZES: [u8; 3] = [9, 13, 19];

/// The rule set a game is played under.
///
/// | rule set        | scoring                      | ko                 | komi |
/// |-----------------|------------------------------|--------------------|------|
/// | Chinese         | area (stones + territory)    | positional superko | 7.5  |
/// | Japanese/Korean | territory + prisoners        | simple ko only     | 6.5  |
/// | Ancient Chinese | area minus 还棋头 group tax  | simple ko only     | 0    |
///
/// Suicide is illegal under all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RuleSet {
    #[default]
    Chinese,
    JapaneseKorean,
    AncientChinese,
}

impl RuleSet {
    pub const ALL: [RuleSet; 3] = [Self::Chinese, Self::JapaneseKorean, Self::AncientChinese];

    /// Default komi given to White.
    pub const fn default_komi(self) -> f64 {
        match self {
            Self::Chinese => 7.5,
            Self::JapaneseKorean => 6.5,
            Self::AncientChinese => 0.0,
        }
    }

    /// Whether a move may not recreate any earlier whole-board position.
    ///
    /// Japanese/Korean rules have no superko: only the immediate recapture is
    /// banned, and long cycles end the game without a result. Ancient Chinese
    /// rules likewise only knew the simple ko ban.
    pub const fn positional_superko(self) -> bool {
        matches!(self, Self::Chinese)
    }

    /// Display name shown in the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chinese => "中国规则 Chinese",
            Self::JapaneseKorean => "日韩规则 Japanese/Korean",
            Self::AncientChinese => "古法 Ancient Chinese",
        }
    }

    /// Stable lowercase id used in commands and in `GameView::rules`.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Chinese => "chinese",
            Self::JapaneseKorean => "japanese",
            Self::AncientChinese => "ancient",
        }
    }

    /// The OGS `rules` value, or `None` when OGS has no such rule set.
    pub const fn ogs_name(self) -> Option<&'static str> {
        match self {
            Self::Chinese => Some("chinese"),
            Self::JapaneseKorean => Some("japanese"),
            Self::AncientChinese => None,
        }
    }

    /// The SGF `RU` value.
    pub const fn sgf_name(self) -> &'static str {
        match self {
            Self::Chinese => "Chinese",
            Self::JapaneseKorean => "Japanese",
            Self::AncientChinese => "Ancient Chinese",
        }
    }

    /// Parses a rules string from OGS, SGF `RU`, or a user command.
    ///
    /// Unknown names fall back to Chinese, the area-scoring family that also
    /// covers AGA, Ing, and New Zealand counting.
    pub fn from_name(name: &str) -> Self {
        let lower = name.trim().to_ascii_lowercase();
        if lower.contains("ancient") || lower.contains("古") {
            Self::AncientChinese
        } else if lower.contains("japan")
            || lower.contains("korea")
            || lower == "jp"
            || lower == "kr"
            || lower.contains("日")
            || lower.contains("韩")
        {
            Self::JapaneseKorean
        } else {
            Self::Chinese
        }
    }
}

impl std::fmt::Display for RuleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

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
    if !has_liberty(&next_board, col, row) {
        return Err(MoveError::Suicide);
    }

    Ok((next_board, captured))
}

/// Validates a move without modifying the board.
///
/// Returns `Ok(captured_stones)` if the move is valid, where `captured_stones`
/// is the number of opponent stones that would be captured.
/// Returns [`MoveError`] if the move is illegal.
///
/// Same result as [`simulate_move`] but without copying the board: a move
/// captures the opponent groups whose only liberty is this point, and is
/// suicide when it neither captures nor touches an empty point or a friendly
/// group with another liberty.
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
    let mut breathes = false;
    let mut captured = 0;
    let mut counted: Vec<(u8, u16)> = Vec::new();
    for (nc, nr) in board.neighbors(col, row) {
        match board.get(nc, nr) {
            None => breathes = true,
            Some(other) => {
                if counted.contains(&(nc, nr)) {
                    continue;
                }
                let group = collect_group(board, nc, nr);
                let only_here = count_liberties(board, &group) == 1;
                if other == color {
                    breathes |= !only_here;
                } else if only_here {
                    captured += group.len();
                }
                counted.extend(group);
            }
        }
    }
    if captured == 0 && !breathes {
        return Err(MoveError::Suicide);
    }
    Ok(captured)
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
    let n = board.size() as usize;
    let index = |c: u8, r: u16| (r as usize - 1) * n + c as usize;
    // A flat visited grid keeps this linear in the group size; scanning a
    // Vec for every neighbour was quadratic and dominated counting time.
    let mut seen = vec![false; n * n];
    let mut group = Vec::new();
    let mut stack = vec![(col, row)];
    seen[index(col, row)] = true;

    while let Some((c, r)) = stack.pop() {
        group.push((c, r));
        for (nc, nr) in board.neighbors(c, r) {
            let idx = index(nc, nr);
            if !seen[idx] && board.get(nc, nr) == Some(color) {
                seen[idx] = true;
                stack.push((nc, nr));
            }
        }
    }

    group
}

/// Counts liberties (empty adjacent points) of a group.
pub fn count_liberties(board: &Board, group: &[(u8, u16)]) -> usize {
    let n = board.size() as usize;
    let mut seen = vec![false; n * n];
    let mut liberties = 0;
    for &(c, r) in group {
        for (nc, nr) in board.neighbors(c, r) {
            let idx = (nr as usize - 1) * n + nc as usize;
            if !seen[idx] && board.is_empty(nc, nr) {
                seen[idx] = true;
                liberties += 1;
            }
        }
    }
    liberties
}

/// True when the group containing `(col, row)` has at least one liberty.
/// Stops at the first liberty, so it is cheap for the common case.
pub fn has_liberty(board: &Board, col: u8, row: u16) -> bool {
    let Some(color) = board.get(col, row) else {
        return true;
    };
    let n = board.size() as usize;
    let index = |c: u8, r: u16| (r as usize - 1) * n + c as usize;
    let mut seen = vec![false; n * n];
    let mut stack = vec![(col, row)];
    seen[index(col, row)] = true;
    while let Some((c, r)) = stack.pop() {
        for (nc, nr) in board.neighbors(c, r) {
            match board.get(nc, nr) {
                None => return true,
                Some(other) if other == color => {
                    let idx = index(nc, nr);
                    if !seen[idx] {
                        seen[idx] = true;
                        stack.push((nc, nr));
                    }
                }
                Some(_) => {}
            }
        }
    }
    false
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
        if board.get(nc, nr) == Some(opponent) && !has_liberty(board, nc, nr) {
            let group = collect_group(board, nc, nr);
            for &(c, r) in &group {
                board.remove(c, r);
                captured.push((c, r));
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

        // A real ko. Every stone has a liberty before the fight starts.
        // Row 3: . B W .
        // Row 2: B W . W
        // Row 1: . B W .
        b.place(1, 3, Color::Black);
        b.place(0, 2, Color::Black);
        b.place(1, 1, Color::Black);
        b.place(2, 3, Color::White);
        b.place(1, 2, Color::White);
        b.place(3, 2, Color::White);
        b.place(2, 1, Color::White);

        let mut prior = HashSet::new();
        prior.insert(b.grid_signature().to_vec());

        // Black takes the ko at (2,2), capturing the white stone at (1,2).
        assert_eq!(validate_move_with_superko(&b, 2, 2, Color::Black, None, &prior), Ok(1));
        let captured = execute_move(&mut b, 2, 2, Color::Black);
        assert_eq!(captured, vec![(1, 2)]);
        prior.insert(b.grid_signature().to_vec());

        // White cannot immediately retake at (1,2).
        assert_eq!(validate_move(&b, 1, 2, Color::White, Some((1, 2))), Err(MoveError::Ko));

        // With the simple-ko point cleared, retaking would recreate the
        // starting position, which positional superko forbids.
        assert_eq!(
            validate_move_with_superko(&b, 1, 2, Color::White, None, &prior),
            Err(MoveError::Superko)
        );
        // Without superko the retake captures exactly the one ko stone.
        assert_eq!(validate_move(&b, 1, 2, Color::White, None), Ok(1));
    }

    #[test]
    fn placement_rules_are_identical_on_every_supported_size() {
        for size in SUPPORTED_SIZES {
            let n = u16::from(size);
            let last = size - 1;
            let mut b = Board::new(size);
            // Corners, edges and centre are all legal on an empty board.
            for (col, row) in [(0, 1), (last, 1), (0, n), (last, n), (size / 2, n / 2 + 1)] {
                assert_eq!(validate_move(&b, col, row, Color::Black, None), Ok(0), "{size}: {col},{row}");
            }
            // One step past every edge is off the board.
            assert_eq!(validate_move(&b, size, 1, Color::Black, None), Err(MoveError::OffBoard));
            assert_eq!(validate_move(&b, 0, n + 1, Color::Black, None), Err(MoveError::OffBoard));
            assert_eq!(validate_move(&b, 0, 0, Color::Black, None), Err(MoveError::OffBoard));
            // A corner stone has exactly two liberties.
            b.place(last, n, Color::White);
            assert_eq!(count_liberties(&b, &collect_group(&b, last, n)), 2);
            // Filling both is a capture, not suicide.
            b.place(last - 1, n, Color::Black);
            assert_eq!(validate_move(&b, last, n - 1, Color::Black, None), Ok(1));
            // Playing into the emptied corner eye is suicide for White.
            execute_move(&mut b, last, n - 1, Color::Black);
            assert_eq!(validate_move(&b, last, n, Color::White, None), Err(MoveError::Suicide));
        }
    }

    #[test]
    fn fast_validation_matches_full_simulation_on_random_positions() {
        // Fill boards pseudo-randomly and compare validate_move with
        // simulate_move on every point, for both colours.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for size in SUPPORTED_SIZES {
            for _ in 0..20 {
                let mut b = Board::new(size);
                for row in 1..=u16::from(size) {
                    for col in 0..size {
                        match next() % 5 {
                            0 | 1 => b.place(col, row, Color::Black),
                            2 | 3 => b.place(col, row, Color::White),
                            _ => {}
                        }
                    }
                }
                for row in 1..=u16::from(size) {
                    for col in 0..size {
                        for color in [Color::Black, Color::White] {
                            let full = simulate_move(&b, col, row, color, None)
                                .map(|(_, captured)| captured.len());
                            assert_eq!(validate_move(&b, col, row, color, None), full);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rule_sets_have_the_expected_defaults() {
        assert_eq!(RuleSet::Chinese.default_komi(), 7.5);
        assert_eq!(RuleSet::JapaneseKorean.default_komi(), 6.5);
        assert_eq!(RuleSet::AncientChinese.default_komi(), 0.0);
        assert!(RuleSet::Chinese.positional_superko());
        assert!(!RuleSet::JapaneseKorean.positional_superko());
        assert_eq!(RuleSet::AncientChinese.ogs_name(), None);
        assert_eq!(RuleSet::from_name("korean"), RuleSet::JapaneseKorean);
        assert_eq!(RuleSet::from_name("Japanese"), RuleSet::JapaneseKorean);
        assert_eq!(RuleSet::from_name("aga"), RuleSet::Chinese);
        assert_eq!(RuleSet::from_name("ancient"), RuleSet::AncientChinese);
        for rules in RuleSet::ALL {
            assert_eq!(RuleSet::from_name(rules.id()), rules);
            assert_eq!(RuleSet::from_name(rules.sgf_name()), rules);
        }
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
