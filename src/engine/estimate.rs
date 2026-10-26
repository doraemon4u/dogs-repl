//! Territory estimation and score counting.
//!
//! Two related but distinct features live here:
//!
//! * [`estimate_with_dead`] — the local "what does the board look like"
//!   overlay. It is pure geometry plus user-marked dead stones.
//! * [`score`] — an area/territory count over that overlay. This is what the
//!   local `estimate confirm` command reports.
//!
//! Neither of these is the official OGS result. OGS scoring is negotiated with
//! the opponent through `game/removed_stones/*`; see the Session layer.

use super::board::{Board, Color};
use super::rules;
use std::collections::HashSet;

/// Estimate result for a single board point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointEstimate {
    BlackTerritory,
    WhiteTerritory,
    /// Neutral point touching both colors.
    Dame,
    LiveStone(Color),
    /// Still recorded with its original color, so it can be toggled back.
    DeadStone(Color),
}

/// Scoring system used by [`score`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringRules {
    /// Chinese-style area scoring: stones + surrounded territory + komi.
    Area,
    /// Japanese-style territory scoring: territory + prisoners + komi.
    Territory,
}

impl ScoringRules {
    /// Parses the `rules` string OGS sends (`chinese`, `japanese`, ...).
    pub fn from_ogs(rules: &str) -> Self {
        let lower = rules.to_ascii_lowercase();
        if lower.contains("japanese") || lower.contains("korean") {
            Self::Territory
        } else {
            Self::Area
        }
    }
}

/// A counted score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreSummary {
    pub rules: ScoringRules,
    pub black_territory: u32,
    pub white_territory: u32,
    pub dame: u32,
    pub black_stones: u32,
    pub white_stones: u32,
    pub dead_black: u32,
    pub dead_white: u32,
    /// Stones captured by Black (White prisoners).
    pub captures_black: u32,
    /// Stones captured by White (Black prisoners).
    pub captures_white: u32,
    pub komi: f64,
    pub black_points: f64,
    pub white_points: f64,
}

impl ScoreSummary {
    /// Signed margin from Black's point of view.
    pub fn margin(&self) -> f64 {
        self.black_points - self.white_points
    }

    /// A human-readable result such as `B+3.5`, `W+2.5`, or `Draw`.
    pub fn result(&self) -> String {
        let margin = self.margin();
        if margin.abs() < f64::EPSILON {
            "Draw".to_string()
        } else if margin > 0.0 {
            format!("B+{margin:.1}")
        } else {
            format!("W+{:.1}", -margin)
        }
    }
}

impl std::fmt::Display for ScoreSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}  B {:.1} : W {:.1}  (B territory {}, W territory {}, dame {}, dead B/W {}/{})",
            self.result(),
            self.black_points,
            self.white_points,
            self.black_territory,
            self.white_territory,
            self.dame,
            self.dead_black,
            self.dead_white
        )
    }
}

/// Estimates territory with no dead stones marked.
pub fn estimate_territory(board: &Board) -> Vec<Vec<PointEstimate>> {
    estimate_with_dead(board, &HashSet::new())
}

/// Estimates territory, treating `dead` stones as removed for region ownership.
///
/// The returned grid is indexed `[row][col]` with row 0 at the top of the
/// board (row number `size` in Go coordinates).
pub fn estimate_with_dead(
    board: &Board,
    dead: &HashSet<(u8, u16)>,
) -> Vec<Vec<PointEstimate>> {
    let size = board.size() as usize;
    let mut result = vec![vec![PointEstimate::Dame; size]; size];
    let mut visited = vec![vec![false; size]; size];

    for row in 1..=board.size() as u16 {
        for col in 0..board.size() {
            if let Some(color) = board.get(col, row) {
                let r_idx = (board.size() as u16 - row) as usize;
                result[r_idx][col as usize] = if dead.contains(&(col, row)) {
                    PointEstimate::DeadStone(color)
                } else {
                    PointEstimate::LiveStone(color)
                };
            }
        }
    }

    for row in 1..=board.size() as u16 {
        for col in 0..board.size() {
            let index = (row as usize - 1, col as usize);
            if visited[index.0][index.1] {
                continue;
            }
            if board.is_empty(col, row) || dead.contains(&(col, row)) {
                let (region, boundary) = flood_fill(board, dead, col, row, &mut visited);
                let estimate = match boundary.as_slice() {
                    [Color::Black] => PointEstimate::BlackTerritory,
                    [Color::White] => PointEstimate::WhiteTerritory,
                    _ => PointEstimate::Dame,
                };
                for (fc, fr) in region {
                    // Dead stones keep their marker; they are counted as part
                    // of the region below by `ownership_counts` instead.
                    if board.get(fc, fr).is_some() {
                        continue;
                    }
                    let r_idx = (board.size() as u16 - fr) as usize;
                    result[r_idx][fc as usize] = estimate;
                }
            }
        }
    }

    result
}

/// Flood-fills a connected region of empty and dead points.
fn flood_fill(
    board: &Board,
    dead: &HashSet<(u8, u16)>,
    start_col: u8,
    start_row: u16,
    visited: &mut [Vec<bool>],
) -> (Vec<(u8, u16)>, Vec<Color>) {
    let mut region = Vec::new();
    let mut boundary_colors = Vec::new();
    let mut stack = vec![(start_col, start_row)];

    while let Some((col, row)) = stack.pop() {
        let r = row as usize - 1;
        let c = col as usize;
        if visited[r][c] {
            continue;
        }
        if !board.is_empty(col, row) && !dead.contains(&(col, row)) {
            if let Some(color) = board.get(col, row) {
                if !boundary_colors.contains(&color) {
                    boundary_colors.push(color);
                }
            }
            continue;
        }
        visited[r][c] = true;
        region.push((col, row));
        for (nc, nr) in board.neighbors(col, row) {
            let nr_idx = nr as usize - 1;
            let nc_idx = nc as usize;
            if !visited[nr_idx][nc_idx] {
                stack.push((nc, nr));
            }
        }
    }

    (region, boundary_colors)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct OwnershipCounts {
    black: u32,
    white: u32,
    dame: u32,
}

/// Counts region sizes including dead stones, which are removed for ownership
/// but still occupy a physical point that the capturer controls.
fn ownership_counts(board: &Board, dead: &HashSet<(u8, u16)>) -> OwnershipCounts {
    let size = board.size() as usize;
    let mut visited = vec![vec![false; size]; size];
    let mut counts = OwnershipCounts::default();

    for row in 1..=board.size() as u16 {
        for col in 0..board.size() {
            let index = (row as usize - 1, col as usize);
            if visited[index.0][index.1] {
                continue;
            }
            if !board.is_empty(col, row) && !dead.contains(&(col, row)) {
                continue;
            }
            let (region, boundary) = flood_fill(board, dead, col, row, &mut visited);
            let points = region.len() as u32;
            match boundary.as_slice() {
                [Color::Black] => counts.black += points,
                [Color::White] => counts.white += points,
                _ => counts.dame += points,
            }
        }
    }

    counts
}

/// Counts the current position.
///
/// `captures` is `(black_captures, white_captures)`, i.e. stones captured by
/// each color. Dead stones are removed before counting; under territory
/// scoring they become prisoners for the opponent.
pub fn score(
    board: &Board,
    dead: &HashSet<(u8, u16)>,
    komi: f64,
    rules: ScoringRules,
    captures: (u16, u16),
) -> ScoreSummary {
    let estimates = estimate_with_dead(board, dead);
    let mut black_stones = 0u32;
    let mut white_stones = 0u32;
    let mut dead_black = 0u32;
    let mut dead_white = 0u32;

    for row in &estimates {
        for point in row {
            match point {
                PointEstimate::LiveStone(Color::Black) => black_stones += 1,
                PointEstimate::LiveStone(Color::White) => white_stones += 1,
                PointEstimate::DeadStone(Color::Black) => dead_black += 1,
                PointEstimate::DeadStone(Color::White) => dead_white += 1,
                // Territory and dame are counted per region below, so that a
                // dead stone inside a region still counts as that region's
                // point for scoring.
                PointEstimate::BlackTerritory
                | PointEstimate::WhiteTerritory
                | PointEstimate::Dame => {}
            }
        }
    }
    let counts = ownership_counts(board, dead);
    let (black_territory, white_territory, dame) =
        (counts.black, counts.white, counts.dame);

    let (black_points, white_points) = match rules {
        ScoringRules::Area => (
            f64::from(black_stones + black_territory),
            f64::from(white_stones + white_territory) + komi,
        ),
        ScoringRules::Territory => {
            let black_prisoners = u32::from(captures.0) + dead_white;
            let white_prisoners = u32::from(captures.1) + dead_black;
            (
                f64::from(black_territory) + f64::from(black_prisoners),
                f64::from(white_territory) + f64::from(white_prisoners) + komi,
            )
        }
    };

    ScoreSummary {
        rules,
        black_territory,
        white_territory,
        dame,
        black_stones,
        white_stones,
        dead_black,
        dead_white,
        captures_black: u32::from(captures.0),
        captures_white: u32::from(captures.1),
        komi,
        black_points,
        white_points,
    }
}

/// Returns the connected group of same-colored stones containing `coord`.
///
/// Empty points have no group and return an empty vector, which lets the
/// estimate mode ignore clicks on empty intersections.
pub fn connected_stone_group(board: &Board, coord: (u8, u16)) -> Vec<(u8, u16)> {
    if board.get(coord.0, coord.1).is_none() {
        return Vec::new();
    }
    rules::collect_group(board, coord.0, coord.1)
}

/// Toggles the dead marker for the whole connected group at `coord`.
///
/// If every stone in the group is already marked dead, the marker is removed;
/// otherwise the whole group is marked dead. Empty points are a no-op.
pub fn toggle_dead_group(
    board: &Board,
    dead: &mut HashSet<(u8, u16)>,
    coord: (u8, u16),
) -> bool {
    let group = connected_stone_group(board, coord);
    if group.is_empty() {
        return false;
    }
    let all_dead = group.iter().all(|point| dead.contains(point));
    if all_dead {
        for point in group {
            dead.remove(&point);
        }
    } else {
        for point in group {
            dead.insert(point);
        }
    }
    true
}

/// Default number of Monte-Carlo endgame playouts for automatic dead-stone detection.
pub const DEFAULT_PLAYOUTS: usize = 64;

/// Survival probability threshold below which a stone group is classified as dead.
const DEAD_SURVIVAL_THRESHOLD: f64 = 0.36;

/// Automatically detects dead stones using Monte-Carlo random endgame playouts
/// (design §10.2, modelled after `goban`'s Monte-Carlo estimator).
///
/// The simulation is pure Engine code (no I/O, deterministic seed derived from
/// the board position) and protects single-point true eyes during playouts so
/// live two-eyed groups stay alive while isolated/dead stones inside opponent
/// territory are captured.
pub fn auto_detect_dead(board: &Board, playouts: usize) -> HashSet<(u8, u16)> {
    let size = board.size();
    if size == 0 || board.stone_count() == 0 {
        return HashSet::new();
    }
    let playouts = playouts.max(8);
    let n = size as usize;
    let mut black_owned = vec![vec![0usize; n]; n];
    let mut white_owned = vec![vec![0usize; n]; n];
    let mut rng = Xorshift64::from_board(board);

    for i in 0..playouts {
        let start_color = if i % 2 == 0 {
            Color::Black
        } else {
            Color::White
        };
        let sim = run_playout(board, start_color, &mut rng);
        let territory = estimate_territory(&sim);
        for row in 1..=size as u16 {
            let r_idx = (size as u16 - row) as usize;
            for col in 0..size {
                let c_idx = col as usize;
                match territory[r_idx][c_idx] {
                    PointEstimate::LiveStone(Color::Black) | PointEstimate::BlackTerritory => {
                        black_owned[r_idx][c_idx] += 1;
                    }
                    PointEstimate::LiveStone(Color::White) | PointEstimate::WhiteTerritory => {
                        white_owned[r_idx][c_idx] += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    let mut dead = HashSet::new();
    let mut visited = HashSet::new();

    for row in 1..=size as u16 {
        for col in 0..size {
            let Some(color) = board.get(col, row) else {
                continue;
            };
            if visited.contains(&(col, row)) {
                continue;
            }
            let group = connected_stone_group(board, (col, row));
            for &pt in &group {
                visited.insert(pt);
            }

            let mut survival_sum = 0.0;
            for &(gc, gr) in &group {
                let r_idx = (size as u16 - gr) as usize;
                let c_idx = gc as usize;
                let owned = match color {
                    Color::Black => black_owned[r_idx][c_idx],
                    Color::White => white_owned[r_idx][c_idx],
                };
                survival_sum += owned as f64 / playouts as f64;
            }
            let avg_survival = survival_sum / group.len() as f64;
            if avg_survival < DEAD_SURVIVAL_THRESHOLD {
                for pt in group {
                    dead.insert(pt);
                }
            }
        }
    }

    dead
}

/// Runs a single light playout until neither player has a non-eye legal move.
fn run_playout(board: &Board, start_color: Color, rng: &mut Xorshift64) -> Board {
    let mut sim = board.clone();
    let size = sim.size();
    let max_steps = (size as usize) * (size as usize) * 2 + 16;
    let mut color = start_color;
    let mut ko_point: Option<(u8, u16)> = None;
    let mut consecutive_passes = 0u8;

    let mut empty_points: Vec<(u8, u16)> = Vec::with_capacity((size as usize) * (size as usize));

    for _ in 0..max_steps {
        empty_points.clear();
        for row in 1..=size as u16 {
            for col in 0..size {
                if sim.is_empty(col, row) {
                    empty_points.push((col, row));
                }
            }
        }

        // Shuffle candidate order with Fisher-Yates so the first legal non-eye
        // move is chosen uniformly, while preferring immediate captures when available.
        let len = empty_points.len();
        for idx in 0..len {
            let swap_idx = idx + (rng.next_u64() as usize % (len - idx));
            empty_points.swap(idx, swap_idx);
        }

        let mut chosen: Option<(u8, u16)> = None;
        for &(col, row) in &empty_points {
            if is_true_eye(&sim, col, row, color) {
                continue;
            }
            if let Ok(captures) = rules::validate_move(&sim, col, row, color, ko_point) {
                if captures > 0 {
                    chosen = Some((col, row));
                    break;
                }
                if chosen.is_none() {
                    chosen = Some((col, row));
                }
            }
        }

        if let Some((col, row)) = chosen {
            let captured = rules::execute_move(&mut sim, col, row, color);
            ko_point = if captured.len() == 1 {
                let group = rules::collect_group(&sim, col, row);
                if group.len() == 1 && rules::count_liberties(&sim, &group) == 1 {
                    Some(captured[0])
                } else {
                    None
                }
            } else {
                None
            };
            consecutive_passes = 0;
        } else {
            ko_point = None;
            consecutive_passes += 1;
            if consecutive_passes >= 2 {
                break;
            }
        }
        color = color.opponent();
    }

    sim
}

/// Checks whether `(col, row)` is a single-point eye for `color` that should
/// not be self-filled during a playout.
fn is_true_eye(board: &Board, col: u8, row: u16, color: Color) -> bool {
    let neighbors = board.neighbors(col, row);
    if neighbors.is_empty() {
        return false;
    }
    for &(nc, nr) in &neighbors {
        if board.get(nc, nr) != Some(color) {
            return false;
        }
    }

    let size = board.size() as i16;
    let deltas: [(i16, i16); 4] = [(-1, -1), (-1, 1), (1, -1), (1, 1)];
    let mut diag_total = 0usize;
    let mut friendly_diag = 0usize;
    for (dc, dr) in deltas {
        let nc = col as i16 + dc;
        let nr = row as i16 + dr;
        if nc >= 0 && nc < size && nr >= 1 && nr <= size {
            diag_total += 1;
            if board.get(nc as u8, nr as u16) == Some(color) {
                friendly_diag += 1;
            }
        }
    }

    if diag_total == 4 {
        friendly_diag >= 3
    } else {
        friendly_diag == diag_total
    }
}

/// Small deterministic PRNG seeded from a board position.
struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    fn from_board(board: &Board) -> Self {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ (board.size() as u64);
        for row in 1..=board.size() as u16 {
            for col in 0..board.size() {
                let code: u64 = match board.get(col, row) {
                    None => 1,
                    Some(Color::Black) => 2,
                    Some(Color::White) => 3,
                };
                hash ^= code.wrapping_mul(((row as u64) << 8) ^ (col as u64));
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        if hash == 0 {
            hash = 0x9e37_79b9_7f4a_7c15;
        }
        Self { state: hash }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead(points: &[(u8, u16)]) -> HashSet<(u8, u16)> {
        points.iter().copied().collect()
    }

    #[test]
    fn empty_board_all_dame() {
        let board = Board::new(9);
        let result = estimate_territory(&board);
        for row in &result {
            for point in row {
                assert_eq!(*point, PointEstimate::Dame);
            }
        }
    }

    #[test]
    fn black_territory() {
        let mut board = Board::new(9);
        for col in 0..9 {
            board.place(col, 9, Color::Black);
        }
        let result = estimate_territory(&board);
        assert_eq!(result[1][0], PointEstimate::BlackTerritory);
    }

    #[test]
    fn white_territory() {
        let mut board = Board::new(9);
        for col in 0..9 {
            board.place(col, 1, Color::White);
        }
        let result = estimate_territory(&board);
        assert_eq!(result[7][0], PointEstimate::WhiteTerritory);
    }

    #[test]
    fn dame_between_colors() {
        let mut board = Board::new(9);
        for row in 1..=9 {
            board.place(0, row, Color::Black);
            board.place(8, row, Color::White);
        }
        let result = estimate_territory(&board);
        assert_eq!(result[4][4], PointEstimate::Dame);
    }

    #[test]
    fn a_marked_dead_stone_becomes_territory() {
        let mut board = Board::new(9);
        for col in 0..9 {
            board.place(col, 9, Color::Black);
        }
        board.place(4, 8, Color::White);
        let dead = dead(&[(4, 8)]);
        let result = estimate_with_dead(&board, &dead);
        assert_eq!(result[1][4], PointEstimate::DeadStone(Color::White));
        let summary = score(&board, &dead, 0.0, ScoringRules::Area, (0, 0));
        assert_eq!(summary.dead_white, 1);
        assert_eq!(summary.dead_black, 0);
        // The dead stone's point belongs to Black's region.
        assert_eq!(summary.black_territory, 72);
    }

    #[test]
    fn area_scoring_counts_stones_and_territory() {
        let mut board = Board::new(9);
        for col in 0..9 {
            board.place(col, 9, Color::Black);
        }
        // The whole 8x9 area below the black wall is Black territory.
        let summary = score(&board, &HashSet::new(), 0.0, ScoringRules::Area, (0, 0));
        assert_eq!(summary.black_stones, 9);
        assert_eq!(summary.black_territory, 72);
        assert_eq!(summary.black_points, 81.0);
        assert_eq!(summary.white_points, 0.0);
        assert!(summary.result().starts_with("B+"));
    }

    #[test]
    fn territory_scoring_counts_prisoners_and_komi() {
        let mut board = Board::new(9);
        for col in 0..9 {
            board.place(col, 9, Color::Black);
        }
        let summary = score(
            &board,
            &HashSet::new(),
            6.5,
            ScoringRules::Territory,
            (2, 0),
        );
        assert_eq!(summary.black_territory, 72);
        assert_eq!(summary.black_points, 74.0);
        assert_eq!(summary.white_points, 6.5);
    }

    #[test]
    fn draw_detection_uses_komi() {
        let board = Board::new(9);
        let summary = score(&board, &HashSet::new(), 0.0, ScoringRules::Area, (0, 0));
        assert_eq!(summary.result(), "Draw");
    }

    #[test]
    fn rules_strings_map_to_scoring_systems() {
        assert_eq!(ScoringRules::from_ogs("chinese"), ScoringRules::Area);
        assert_eq!(ScoringRules::from_ogs("aga"), ScoringRules::Area);
        assert_eq!(ScoringRules::from_ogs("japanese"), ScoringRules::Territory);
        assert_eq!(ScoringRules::from_ogs("Korean"), ScoringRules::Territory);
    }

    #[test]
    fn toggling_a_group_marks_and_unmarks_it() {
        let mut board = Board::new(9);
        board.place(3, 3, Color::Black);
        board.place(3, 4, Color::Black);
        board.place(6, 6, Color::White);
        let mut dead = HashSet::new();
        assert!(toggle_dead_group(&board, &mut dead, (3, 3)));
        assert_eq!(dead.len(), 2);
        assert!(dead.contains(&(3, 4)));
        assert!(toggle_dead_group(&board, &mut dead, (3, 4)));
        assert!(dead.is_empty());
        assert!(!toggle_dead_group(&board, &mut dead, (0, 0)));
    }

    #[test]
    fn connected_group_for_empty_points_is_empty() {
        let board = Board::new(9);
        assert!(connected_stone_group(&board, (4, 4)).is_empty());
    }

    #[test]
    fn auto_detect_dead_marks_surrounded_dead_stone_and_keeps_live_wall() {
        let mut board = Board::new(9);
        // Black builds a two-eyed fortress in the bottom-left corner:
        // Eyes at (0,1) and (2,1), surrounded by Black stones on rows 1..=3, cols 0..=3.
        for col in 0..=3 {
            for row in 1..=3 {
                if (col == 0 && row == 1) || (col == 2 && row == 1) {
                    continue;
                }
                board.place(col, row, Color::Black);
            }
        }
        // Place a doomed White stone at (1, 5) tightly boxed in by Black stones
        // with only a single liberty and no eye space.
        board.place(1, 5, Color::White);
        board.place(0, 5, Color::Black);
        board.place(2, 5, Color::Black);
        board.place(1, 4, Color::Black);
        // Leave (1, 6) empty but surround (1, 6) with Black so White cannot escape.
        board.place(0, 6, Color::Black);
        board.place(2, 6, Color::Black);
        board.place(1, 7, Color::Black);

        let detected = auto_detect_dead(&board, DEFAULT_PLAYOUTS);
        assert!(
            detected.contains(&(1, 5)),
            "doomed white stone should be detected as dead: {detected:?}"
        );
        assert!(
            !detected.contains(&(1, 1)),
            "two-eyed black fortress must remain alive: {detected:?}"
        );
    }
}
