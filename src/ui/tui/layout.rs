//! Responsive layout and minimum-size checks (design §9.2-9.3).
//!
//! Minimum sizes are per scene and per board size, never a global constant.
//! Every frame recomputes the split from the current terminal size, so a resize
//! simply changes the inputs to these functions.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Width of one board intersection in characters. Two columns make the visual
/// aspect ratio roughly square on a 1:2 terminal font (design §9.2).
pub const CELL_WIDTH: u16 = 2;
/// Space reserved for row labels and padding around the grid.
pub const BOARD_CHROME_WIDTH: u16 = 7;
/// Minimum width of the right-hand column (moves + chat).
pub const SIDE_PANEL_MIN_WIDTH: u16 = 40;
/// Minimum height of the right-hand column.
pub const SIDE_PANEL_MIN_HEIGHT: u16 = 14;
/// Status line plus input line at the bottom.
pub const INPUT_ROWS: u16 = 2;

/// What the current screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    Lobby,
    Game { board_size: u8 },
}

impl Scene {
    pub fn min_size(self) -> (u16, u16) {
        match self {
            Scene::Lobby => (72, 20),
            Scene::Game { board_size } => {
                // The board gets exactly half the width, so both halves must
                // fit: the smallest board pane and the side column.
                let width = board_pane_width(board_size).max(SIDE_PANEL_MIN_WIDTH) * 2;
                let height = (board_pane_height(board_size) + INPUT_ROWS).max(22);
                (width, height)
            }
        }
    }

    pub fn label(self) -> String {
        match self {
            Scene::Lobby => "lobby".to_string(),
            Scene::Game { board_size } => format!("game {board_size}x{board_size}"),
        }
    }
}

/// Total width of the board pane, including borders.
pub fn board_pane_width(board_size: u8) -> u16 {
    CELL_WIDTH * u16::from(board_size) + BOARD_CHROME_WIDTH
}

/// Total height of the board pane, including the column header and borders.
pub fn board_pane_height(board_size: u8) -> u16 {
    u16::from(board_size) + 3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameLayout {
    pub header: Rect,
    pub board: Rect,
    pub moves: Rect,
    pub chat: Rect,
    pub status: Rect,
    pub input: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LobbyLayout {
    pub header: Rect,
    pub panel: Rect,
    pub games: Rect,
    pub chat: Rect,
    pub status: Rect,
    pub input: Rect,
}

/// True when the area is large enough to draw anything.
pub fn usable(area: Rect) -> bool {
    area.width > 0 && area.height > 0
}

/// Returns the recommended minimum when `area` is too small.
pub fn too_small(area: Rect, scene: Scene) -> Option<(u16, u16)> {
    if !usable(area) {
        // A zero-sized frame (some terminals emit this while minimised) is
        // skipped by the caller; report the scene minimum for the message.
        return Some(scene.min_size());
    }
    let (min_width, min_height) = scene.min_size();
    (area.width < min_width || area.height < min_height).then_some((min_width, min_height))
}

/// Splits the game screen into header, board, moves, chat, status, and input.
///
/// For 9×9 and 13×13, the board pane is half the window's width. For 19×19,
/// when the window has spare width beyond the minimum side panel width, the
/// board pane expands proportionally (up to 80 columns) so the 19×19 grid can
/// render at full scale instead of sitting small and centered.
pub fn split_game(area: Rect, board_size: u8) -> GameLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(INPUT_ROWS),
        ])
        .split(area);
    let (header, main, bottom) = (vertical[0], vertical[1], vertical[2]);
    let bottom_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(bottom);
    let half = area.width / 2;
    let board_width = if board_size >= 19 && area.width >= 80 + SIDE_PANEL_MIN_WIDTH {
        half.max(80.min(area.width.saturating_sub(SIDE_PANEL_MIN_WIDTH)))
    } else {
        half
    };
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(board_width), Constraint::Min(0)])
        .split(main);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(horizontal[1]);
    GameLayout {
        header,
        board: horizontal[0],
        moves: right[0],
        chat: right[1],
        status: bottom_rows[0],
        input: bottom_rows[1],
    }
}

/// Splits the lobby screen.
pub fn split_lobby(area: Rect) -> LobbyLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(INPUT_ROWS),
        ])
        .split(area);
    let (header, main, bottom) = (vertical[0], vertical[1], vertical[2]);
    let bottom_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(bottom);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(main);
    LobbyLayout {
        header,
        panel: main,
        games: horizontal[0],
        chat: horizontal[1],
        status: bottom_rows[0],
        input: bottom_rows[1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_width_is_dynamic_per_size() {
        assert_eq!(board_pane_width(9), 25);
        assert_eq!(board_pane_width(13), 33);
        assert_eq!(board_pane_width(19), 45);
        assert!(board_pane_width(19) >= 45 && board_pane_width(19) <= 50);
        assert_eq!(board_pane_height(19), 22);
        assert!(board_pane_width(9) < board_pane_width(19));
    }

    #[test]
    fn minimum_size_depends_on_scene_and_board() {
        let lobby = Scene::Lobby.min_size();
        let game9 = Scene::Game { board_size: 9 }.min_size();
        let game19 = Scene::Game { board_size: 19 }.min_size();
        assert!(game9.0 < game19.0);
        assert!(game19.0 > lobby.0 || game19.1 > lobby.1);
        assert!(game19.1 >= 24);
    }

    #[test]
    fn too_small_reports_the_scene_minimum() {
        let small = Rect::new(0, 0, 40, 10);
        assert_eq!(
            too_small(small, Scene::Game { board_size: 19 }),
            Some(Scene::Game { board_size: 19 }.min_size())
        );
        let large = Rect::new(0, 0, 120, 40);
        assert_eq!(too_small(large, Scene::Game { board_size: 19 }), None);
    }

    #[test]
    fn zero_area_is_not_usable() {
        assert!(!usable(Rect::new(0, 0, 0, 0)));
        assert!(!usable(Rect::new(0, 0, 10, 0)));
        assert!(usable(Rect::new(0, 0, 1, 1)));
        assert!(too_small(Rect::new(0, 0, 0, 0), Scene::Lobby).is_some());
    }

    #[test]
    fn game_layout_fits_and_sizes_the_board_per_size() {
        let area = Rect::new(0, 0, 120, 40);
        let layout9 = split_game(area, 9);
        let layout13 = split_game(area, 13);
        let layout = split_game(area, 19);
        assert_eq!(layout9.board.width, 60);
        assert_eq!(layout13.board.width, 60);
        assert_eq!(layout.board.width, 80, "19x19 board pane scales up proportionally");
        assert!(area.width - layout.board.width >= SIDE_PANEL_MIN_WIDTH);
        assert_eq!(layout.board.x, 0);
        assert_eq!(layout.status.height, 1);
        assert_eq!(layout.input.height, 1);
        assert_eq!(layout.status.y, 38);
        assert_eq!(layout.input.y, 39);
        assert!(layout.moves.height > 0);
        assert!(layout.chat.height > 0);
        assert_eq!(
            layout.board.y,
            layout.moves.y,
            "board and right column share a top edge"
        );
        // Everything stays inside the frame.
        assert!(layout.moves.x + layout.moves.width <= area.width);
        assert!(layout.chat.y + layout.chat.height <= 38);
    }

    #[test]
    fn board_width_never_exceeds_the_frame() {
        // Even if a caller skips the minimum check, the board pane must not
        // panic or overflow the hard frame.
        let area = Rect::new(0, 0, 30, 20);
        let layout = split_game(area, 19);
        assert!(layout.board.width <= area.width);
        assert!(layout.board.x + layout.board.width <= area.width);
    }

    #[test]
    fn the_board_pane_is_always_half_the_window() {
        for size in [9u8, 13, 19] {
            let (min_w, min_h) = Scene::Game { board_size: size }.min_size();
            for &(w, h) in &[(min_w, min_h), (100u16, 30u16), (141, 45), (200, 60)] {
                let w = w.max(min_w);
                let h = h.max(min_h);
                let area = Rect::new(0, 0, w, h);
                let layout = split_game(area, size);
                let expected_w = if size >= 19 && w >= 80 + SIDE_PANEL_MIN_WIDTH {
                    (w / 2).max(80.min(w - SIDE_PANEL_MIN_WIDTH))
                } else {
                    w / 2
                };
                assert_eq!(layout.board.width, expected_w, "{size}x{size} in {w}x{h}");
                assert_eq!(layout.moves.x, expected_w, "side column starts after board");
                assert_eq!(layout.moves.width, w - expected_w);
                assert_eq!(layout.board.height, h - 1 - INPUT_ROWS);
                // At the minimum size the side column is still usable.
                assert!(layout.moves.width >= SIDE_PANEL_MIN_WIDTH);
            }
        }
    }

    #[test]
    fn lobby_layout_has_one_outer_panel() {
        let area = Rect::new(0, 0, 100, 30);
        let layout = split_lobby(area);
        // Row 0 is the header bar; the panel fills the rest above the bottom bars.
        assert_eq!(layout.panel, Rect::new(0, 1, 100, 27));
        assert_eq!(layout.status.y, 28);
        assert_eq!(layout.input.y, 29);
        assert!(layout.games.width > 0);
        assert!(layout.chat.width > 0);
    }
}
