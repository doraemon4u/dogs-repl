//! Board rendering with two-character-wide intersections (design §9.2).
//!
//! The same cell glyphs are used by the TUI and the plain-text REPL, so a board
//! looks the same whether it is drawn with `ratatui` or printed to stdout.

use crate::engine::board::{Board, Color};
use crate::engine::estimate::PointEstimate;
use crate::session::store::GameView;
use crate::ui::tui::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Fixed width reserved for the row labels.
pub const LABEL_WIDTH: u16 = 3;

/// Renders the board as plain text with the current stones.
pub fn render_text(game: &GameView) -> String {
    render_text_with_overlay(game, None)
}

/// Renders the board as plain text, optionally using an estimate overlay.
pub fn render_text_with_overlay(
    game: &GameView,
    overlay: Option<&[Vec<PointEstimate>]>,
) -> String {
    let size = game.size;
    let mut lines = Vec::with_capacity(size as usize + 2);
    let mut header = " ".repeat(LABEL_WIDTH as usize);
    for col in 0..size {
        header.push(Board::letter_for_col(col).unwrap_or('?'));
        header.push(' ');
    }
    lines.push(header);
    let last_move = game.last_move();
    for row_index in 0..size {
        let row = u16::from(size) - u16::from(row_index);
        let mut line = format!("{row:>width$} ", width = LABEL_WIDTH as usize - 1);
        for col in 0..size {
            let point = overlay.and_then(|grid| {
                grid.get(row_index as usize)
                    .and_then(|line| line.get(col as usize))
                    .copied()
            });
            let cell = text_cell(game.board(), size, col, row, point);
            line.push_str(&cell);
        }
        lines.push(line);
    }
    let captures = format!(
        "Black captures {} · White captures {} · move {}",
        game.game.captures(Color::Black),
        game.game.captures(Color::White),
        game.move_number()
    );
    lines.push(captures);
    if let Some(coord) = last_move {
        lines.push(format!("last move: {}", coord.to_gtp()));
    }
    lines.join("\n")
}

/// Returns the fixed two-column text cell for one point.
pub fn text_cell(
    board: &Board,
    size: u8,
    col: u8,
    row: u16,
    overlay: Option<PointEstimate>,
) -> String {
    let row_index = u16::from(size) - row;
    match overlay {
        Some(PointEstimate::DeadStone(_)) => "╳ ".to_string(),
        Some(PointEstimate::BlackTerritory) => "■ ".to_string(),
        Some(PointEstimate::WhiteTerritory) => "□ ".to_string(),
        Some(PointEstimate::Dame) => "· ".to_string(),
        Some(PointEstimate::LiveStone(Color::Black)) => "●".to_string(),
        Some(PointEstimate::LiveStone(Color::White)) => "○".to_string(),
        _ => match board.get(col, row) {
            Some(Color::Black) => "●".to_string(),
            Some(Color::White) => "○".to_string(),
            None => {
                let glyph = intersection(size, col, row_index as u8);
                if col + 1 < size {
                    format!("{glyph}─")
                } else {
                    format!("{glyph} ")
                }
            }
        },
    }
}

fn intersection(size: u8, col: u8, row_index: u8) -> char {
    let left = col > 0;
    let right = col + 1 < size;
    let up = row_index > 0;
    let down = row_index + 1 < size;
    match (left, right, up, down) {
        (true, true, true, true) => '┼',
        (false, true, true, true) => '├',
        (true, false, true, true) => '┤',
        (true, true, false, true) => '┬',
        (true, true, true, false) => '┴',
        (false, true, false, true) => '┌',
        (true, false, false, true) => '┐',
        (false, true, true, false) => '└',
        (true, false, true, false) => '┘',
        (true, true, false, false) => '─',
        (false, false, true, true) => '│',
        _ => '·',
    }
}

/// Draws the board into an already-cleared inner area.
///
/// `area` is the area inside the pane border. The caller must have checked the
/// minimum size; this function is still defensive about short areas.
pub fn draw_board(
    buffer: &mut Buffer,
    area: Rect,
    game: &GameView,
    theme: &Theme,
    focused: bool,
) {
    let size = game.size;
    let needed_width = LABEL_WIDTH + u16::from(size) * 2;
    let needed_height = u16::from(size) + 1;
    if area.width < needed_width || area.height < needed_height {
        let text = "board area too small";
        buffer.set_string(
            area.x,
            area.y,
            text,
            Style::default().fg(theme.error),
        );
        return;
    }

    let muted = theme.dim(theme.board);
    let header_style = Style::default().fg(if focused {
        theme.focus(theme.board)
    } else {
        theme.board
    });
    for col in 0..size {
        if let Some(letter) = Board::letter_for_col(col) {
            let x = area.x + LABEL_WIDTH + u16::from(col) * 2;
            buffer.set_string(x, area.y, &letter.to_string(), header_style);
        }
    }

    let overlay = game
        .estimate_active
        .then(|| game.estimate_overlay())
        .unwrap_or_default();
    let last_move = game.last_move();
    for row_index in 0..size {
        let row = u16::from(size) - u16::from(row_index);
        let y = area.y + 1 + u16::from(row_index);
        let label = format!("{row:>width$}", width = LABEL_WIDTH as usize);
        buffer.set_string(area.x, y, &label, Style::default().fg(muted));
        for col in 0..size {
            let x = area.x + LABEL_WIDTH + u16::from(col) * 2;
            let estimate = if game.estimate_active {
                overlay
                    .get(row_index as usize)
                    .and_then(|line| line.get(col as usize))
                    .copied()
            } else {
                None
            };
            let (cell, mut style) = cell_style(game, theme, col, row, estimate);
            if last_move
                .map(|coord| coord.col == col && coord.row == row)
                .unwrap_or(false)
            {
                style = style.fg(theme.last_move).add_modifier(Modifier::REVERSED);
            }
            buffer.set_string(x, y, &cell, style);
        }
    }
}

fn cell_style(
    game: &GameView,
    theme: &Theme,
    col: u8,
    row: u16,
    estimate: Option<PointEstimate>,
) -> (String, Style) {
    let size = game.size;
    let row_index = u16::from(size) - row;
    match estimate {
        Some(PointEstimate::DeadStone(_color)) => (
            "╳ ".to_string(),
            Style::default()
                .fg(theme.dead_stone)
                .add_modifier(Modifier::DIM),
        ),
        Some(PointEstimate::BlackTerritory) => (
            "■ ".to_string(),
            Style::default().fg(theme.territory_black),
        ),
        Some(PointEstimate::WhiteTerritory) => (
            "□ ".to_string(),
            Style::default().fg(theme.territory_white),
        ),
        Some(PointEstimate::Dame) => (
            "· ".to_string(),
            Style::default().fg(theme.muted),
        ),
        Some(PointEstimate::LiveStone(color)) => stone_cell(color, theme),
        None => match game.board().get(col, row) {
            Some(color) => stone_cell(color, theme),
            None => {
                let glyph = intersection(size, col, u8::try_from(row_index).unwrap_or(0));
                let cell = if col + 1 < size {
                    format!("{glyph}─")
                } else {
                    format!("{glyph} ")
                };
                (cell, Style::default().fg(theme.dim(theme.board)))
            }
        },
    }
}

fn stone_cell(color: Color, theme: &Theme) -> (String, Style) {
    match color {
        Color::Black => (
            "●".to_string(),
            Style::default()
                .fg(theme.stone_black)
                .add_modifier(Modifier::BOLD),
        ),
        Color::White => (
            "○".to_string(),
            Style::default()
                .fg(theme.stone_white)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::coord::Coord;
    use crate::engine::estimate::estimate_with_dead;
    use std::collections::HashSet;

    fn game() -> GameView {
        let mut game = GameView::empty(1, 9);
        game.game.play(Coord::new(2, 2)).unwrap();
        game.game.play(Coord::new(6, 6)).unwrap();
        game
    }

    #[test]
    fn text_board_has_two_character_cells_and_labels() {
        let empty = GameView::empty(1, 9);
        let text = render_text(&empty);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0].trim(), "A B C D E F G H J");
        // Row label + 9 empty cells of 2 characters.
        assert_eq!(lines[1].chars().count(), 3 + 9 * 2);
        let text = render_text(&game());
        assert!(text.contains('●'));
        assert!(text.contains('○'));
        assert!(text.contains("Black captures 0"));
    }

    #[test]
    fn text_board_top_row_is_the_highest_row_number() {
        let text = render_text(&game());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].trim_start().starts_with('9'));
        assert!(lines[9].trim_start().starts_with('1'));
    }

    #[test]
    fn estimate_overlay_replaces_empty_grid_content() {
        let mut game = game();
        game.estimate_active = true;
        let mut dead = HashSet::new();
        dead.insert((2, 2));
        let overlay = estimate_with_dead(game.board(), &dead);
        let text = render_text_with_overlay(&game, Some(&overlay));
        assert!(text.contains('╳'));
        assert!(text.contains('■') || text.contains('□') || text.contains('·'));
    }

    #[test]
    fn intersection_glyphs_have_arms() {
        assert_eq!(intersection(9, 4, 4), '┼');
        assert_eq!(intersection(9, 0, 0), '┌');
        assert_eq!(intersection(9, 8, 8), '┘');
        assert_eq!(intersection(9, 8, 0), '┐');
        assert_eq!(intersection(9, 0, 8), '└');
    }

    #[test]
    fn text_cell_is_always_two_columns_for_empty_points() {
        let board = Board::new(9);
        for col in 0..9 {
            for row in 1..=9 {
                let cell = text_cell(&board, 9, col, row, None);
                assert_eq!(cell.chars().count(), 2, "cell {col},{row}: {cell:?}");
            }
        }
    }
}
