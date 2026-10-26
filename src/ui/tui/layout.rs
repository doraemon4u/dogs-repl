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
                let width = board_pane_width(board_size) + SIDE_PANEL_MIN_WIDTH;
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
    pub board: Rect,
    pub moves: Rect,
    pub chat: Rect,
    pub status: Rect,
    pub input: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LobbyLayout {
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

/// Splits the game screen into board, moves, chat, status, and input.
pub fn split_game(area: Rect, board_size: u8) -> GameLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(INPUT_ROWS)])
        .split(area);
    let (main, bottom) = (vertical[0], vertical[1]);
    let bottom_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(bottom);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(board_pane_width(board_size).min(main.width)),
            Constraint::Min(0),
        ])
        .split(main);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(horizontal[1]);
    GameLayout {
        board: horizontal[0],
        moves: right[0],
        chat: right[1],
        status: bottom_rows[0],
        input: bottom_rows[1],
    }
}

/// Splits the lobby screen. The lobby deliberately shares one outer pane
/// (design §9.5): browsing is a single activity, so the left info and right
/// chat are separated by a divider rather than independent frames.
pub fn split_lobby(area: Rect) -> LobbyLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(INPUT_ROWS)])
        .split(area);
    let (main, bottom) = (vertical[0], vertical[1]);
    let bottom_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(bottom);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(main);
    LobbyLayout {
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
    fn game_layout_fits_and_uses_a_fixed_board() {
        let area = Rect::new(0, 0, 120, 40);
        let layout = split_game(area, 19);
        assert_eq!(layout.board.width, 45);
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
    fn lobby_layout_has_one_outer_panel() {
        let area = Rect::new(0, 0, 100, 30);
        let layout = split_lobby(area);
        assert_eq!(layout.panel, Rect::new(0, 0, 100, 28));
        assert_eq!(layout.status.y, 28);
        assert_eq!(layout.input.y, 29);
        assert!(layout.games.width > 0);
        assert!(layout.chat.width > 0);
    }
}
