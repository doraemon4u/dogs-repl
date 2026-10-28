//! Board rendering with two-character-wide intersections (design §9.2).
//!
//! The same cell glyphs are used by the TUI and the plain-text REPL, so a board
//! looks the same whether it is drawn with `ratatui` or printed to stdout.

use crate::engine::board::{Board, Color};
use crate::engine::coord::Coord;
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
    let ko_point = game.ko_point();
    for row_index in 0..size {
        let row = u16::from(size) - u16::from(row_index);
        let mut line = format!("{row:>width$} ", width = LABEL_WIDTH as usize - 1);
        for col in 0..size {
            let point = overlay.and_then(|grid| {
                grid.get(row_index as usize)
                    .and_then(|line| line.get(col as usize))
                    .copied()
            });
            let cell = if point.is_none() && ko_point == Some((col, row)) && game.board().is_empty(col, row) {
                "□ ".to_string()
            } else {
                text_cell(game.board(), size, col, row, point)
            };
            line.push_str(&cell);
        }
        lines.push(line);
    }
    let captures = format!(
        "Black captures {} · White captures {} · move {}",
        game.active_game_ref().captures(Color::Black),
        game.active_game_ref().captures(Color::White),
        game.displayed_move_number()
    );
    lines.push(captures);
    if let Some(coord) = last_move {
        lines.push(format!("last move: {}", coord.to_gtp()));
    }
    if let Some((kc, kr)) = ko_point {
        lines.push(format!("ko point: {}", crate::engine::coord::Coord::new(kc, kr).to_gtp()));
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
        Some(PointEstimate::LiveStone(Color::Black)) => {
            if col + 1 < size { "●─".to_string() } else { "● ".to_string() }
        }
        Some(PointEstimate::LiveStone(Color::White)) => {
            if col + 1 < size { "○─".to_string() } else { "○ ".to_string() }
        }
        _ => match board.get(col, row) {
            Some(Color::Black) => {
                if col + 1 < size { "●─".to_string() } else { "● ".to_string() }
            }
            Some(Color::White) => {
                if col + 1 < size { "○─".to_string() } else { "○ ".to_string() }
            }
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

/// Returns true if `(col, row)` (0-based col, 1-based row from bottom) is a star point (hoshi)
/// on a 9x9, 13x13, or 19x19 board.
pub fn is_hoshi(size: u8, col: u8, row: u16) -> bool {
    if row == 0 || row > u16::from(size) {
        return false;
    }
    let row_index = (u16::from(size) - row) as u8;
    is_hoshi_index(size, col, row_index)
}

fn is_hoshi_index(size: u8, col: u8, row_index: u8) -> bool {
    match size {
        9 => {
            ((col == 2 || col == 6) && (row_index == 2 || row_index == 6))
                || (col == 4 && row_index == 4)
        }
        13 => {
            ((col == 3 || col == 9) && (row_index == 3 || row_index == 9))
                || (col == 6 && row_index == 6)
        }
        19 => {
            let pts = [3u8, 9, 15];
            pts.contains(&col) && pts.contains(&row_index)
        }
        _ => false,
    }
}

fn intersection(size: u8, col: u8, row_index: u8) -> char {
    if is_hoshi_index(size, col, row_index) {
        return '╋';
    }
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

/// Returns the stone horizontal and vertical half-radii `(rx, ry)` for a given `(col_step, row_step)`.
/// Stones are rendered as pure circular glyphs directly on intersections (rx = 0, ry = 0).
pub fn stone_radii(_col_step: u16, _row_step: u16) -> (u16, u16) {
    (0, 0)
}

/// Computes optimal `(col_step, row_step)` enforcing a strict 2:1 character aspect ratio
/// (`col_step == row_step * 2`) so 9x9, 13x13, and 19x19 boards are ALWAYS rendered as a
/// true 1:1 geometric square (正四边形) in terminal character cells, never a rectangle.
pub fn compute_board_steps(area: Rect, size: u8) -> (u16, u16) {
    if size <= 1 {
        return (2, 1);
    }
    let gaps = u16::from(size - 1);

    let mut best_row_step = 1u16;
    for r in 1..=6 {
        let c = r * 2;
        let needed_w = LABEL_WIDTH + gaps * c + 1;
        let needed_h = gaps * r + 1;
        if needed_w <= area.width && needed_h <= area.height {
            best_row_step = r;
        } else {
            break;
        }
    }

    (best_row_step * 2, best_row_step)
}

#[derive(Debug, Clone, Copy)]
struct BoardMetrics {
    col_step: u16,
    row_step: u16,
    stone_rx: u16,
    stone_ry: u16,
    origin_x: u16,
    origin_y: u16,
    grid_x0: u16,
    grid_y0: u16,
    grid_w: u16,
    grid_h: u16,
    total_w: u16,
    total_h: u16,
    show_top_coords: bool,
    show_bottom_coords: bool,
    show_right_coords: bool,
}

fn compute_metrics(area: Rect, size: u8) -> Option<BoardMetrics> {
    if area.width == 0 || area.height == 0 || size == 0 {
        return None;
    }
    let (col_step, row_step) = compute_board_steps(area, size);
    let (stone_rx, stone_ry) = stone_radii(col_step, row_step);
    let gaps = u16::from(size.saturating_sub(1));
    let grid_w = gaps * col_step;
    let grid_h = gaps * row_step;

    let min_w = LABEL_WIDTH + stone_rx + grid_w + stone_rx + 1;
    let min_h = grid_h + 1;
    if area.width < min_w || area.height < min_h {
        return None;
    }

    let show_top_coords = area.height >= grid_h + 2;
    let top_pad = if area.height >= grid_h + 2 + stone_ry * 2 {
        1 + stone_ry
    } else if show_top_coords {
        1
    } else {
        0
    };
    let show_bottom_coords = area.height >= top_pad + grid_h + 1 + stone_ry + 1;
    let show_right_coords = area.width >= min_w + LABEL_WIDTH;

    let total_w = if show_right_coords {
        min_w + LABEL_WIDTH
    } else {
        min_w
    };
    let total_h = top_pad
        + grid_h
        + 1
        + if show_bottom_coords {
            stone_ry + 1
        } else {
            0
        };

    let origin_x = area.x + area.width.saturating_sub(total_w) / 2;
    let origin_y = area.y + area.height.saturating_sub(total_h) / 2;
    let grid_x0 = origin_x + LABEL_WIDTH + stone_rx;
    let grid_y0 = origin_y + top_pad;

    Some(BoardMetrics {
        col_step,
        row_step,
        stone_rx,
        stone_ry,
        origin_x,
        origin_y,
        grid_x0,
        grid_y0,
        grid_w,
        grid_h,
        total_w,
        total_h,
        show_top_coords,
        show_bottom_coords,
        show_right_coords,
    })
}

/// Resolves a mouse click (click_x, click_y) inside `area` to a Go board coordinate.
pub fn board_coord_at(area: Rect, size: u8, click_x: u16, click_y: u16) -> Option<Coord> {
    let m = compute_metrics(area, size)?;

    if click_x + m.col_step / 2 < m.grid_x0 || click_x > m.grid_x0 + m.grid_w + m.col_step / 2 {
        return None;
    }
    if click_y + m.row_step / 2 < m.grid_y0 || click_y > m.grid_y0 + m.grid_h + m.row_step / 2 {
        return None;
    }

    let rel_x = click_x.saturating_sub(m.grid_x0);
    let col = (rel_x + m.col_step / 2) / m.col_step;
    let rel_y = click_y.saturating_sub(m.grid_y0);
    let row_idx = (rel_y + m.row_step / 2) / m.row_step;

    if col < u16::from(size) && row_idx < u16::from(size) {
        let actual_row = u16::from(size) - row_idx;
        Some(Coord::new(col as u8, actual_row))
    } else {
        None
    }
}

/// Draws the board into an already-cleared inner area.
pub fn draw_board(
    buffer: &mut Buffer,
    area: Rect,
    game: &GameView,
    theme: &Theme,
    focused: bool,
) {
    let size = game.size;
    let Some(m) = compute_metrics(area, size) else {
        if area.width > 0 && area.height > 0 {
            buffer.set_string(
                area.x,
                area.y,
                "board area too small",
                Style::default().fg(theme.error),
            );
        }
        return;
    };

    let muted = theme.dim(theme.board);
    let header_style = Style::default().fg(if focused {
        theme.focus(theme.board)
    } else {
        theme.board
    });
    let grid_line_style = Style::default().fg(muted);

    // Pass 0: Draw an outer line box frame enclosing the board (外边用线条框绘制)
    let fx0 = m.origin_x.saturating_sub(1);
    let fy0 = m.origin_y.saturating_sub(1);
    let fx1 = (m.origin_x + m.total_w).min(area.x + area.width.saturating_sub(1));
    let fy1 = (m.origin_y + m.total_h).min(area.y + area.height.saturating_sub(1));
    if fx0 >= area.x && fx1 > fx0 + 2 && fy0 >= area.y && fy1 > fy0 + 2 {
        let border_st = Style::default().fg(if focused { theme.focus(theme.board) } else { muted });
        buffer.set_string(fx0, fy0, "┏", border_st);
        buffer.set_string(fx1, fy0, "┓", border_st);
        buffer.set_string(fx0, fy1, "┗", border_st);
        buffer.set_string(fx1, fy1, "┛", border_st);
        for cx in (fx0 + 1)..fx1 {
            buffer.set_string(cx, fy0, "━", border_st);
            buffer.set_string(cx, fy1, "━", border_st);
        }
        for cy in (fy0 + 1)..fy1 {
            buffer.set_string(fx0, cy, "┃", border_st);
            buffer.set_string(fx1, cy, "┃", border_st);
        }
    }

    // Pass 1: Coordinate labels & full board grid
    for col in 0..size {
        if let Some(letter) = Board::letter_for_col(col) {
            let x = m.grid_x0 + u16::from(col) * m.col_step;
            let s_let = letter.to_string();
            if m.show_top_coords {
                buffer.set_string(x, m.origin_y, &s_let, header_style);
            }
            if m.show_bottom_coords {
                let bottom_y = m.grid_y0 + m.grid_h + m.stone_ry + 1;
                if bottom_y < area.y + area.height {
                    buffer.set_string(x, bottom_y, &s_let, header_style);
                }
            }
        }
    }

    for row_index in 0..size {
        let row = u16::from(size) - u16::from(row_index);
        let y = m.grid_y0 + u16::from(row_index) * m.row_step;
        let label = format!("{row:>width$} ", width = LABEL_WIDTH as usize - 1);
        buffer.set_string(m.origin_x, y, &label, Style::default().fg(muted));
        if m.show_right_coords {
            let right_label = format!(" {row:<width$}", width = LABEL_WIDTH as usize - 1);
            let right_x = m.grid_x0 + m.grid_w + m.stone_rx + 1;
            if right_x + LABEL_WIDTH <= area.x + area.width {
                buffer.set_string(right_x, y, &right_label, Style::default().fg(muted));
            }
        }

        for col in 0..size {
            let x = m.grid_x0 + u16::from(col) * m.col_step;
            let glyph = intersection(size, col, row_index);
            let fg = if is_hoshi_index(size, col, row_index) {
                theme.board
            } else {
                muted
            };
            buffer.set_string(x, y, glyph.to_string(), Style::default().fg(fg));

            if col + 1 < size {
                for i in 1..m.col_step {
                    buffer.set_string(x + i, y, "─", grid_line_style);
                }
            }
            if row_index + 1 < size && m.row_step > 1 {
                for k in 1..m.row_step {
                    buffer.set_string(x, y + k, "│", grid_line_style);
                }
            }
        }
    }

    // Pass 2: Stones, Ko markers, and Estimate overlays drawn on top of the grid
    let overlay = game
        .estimate_active
        .then(|| game.estimate_overlay())
        .unwrap_or_default();
    let last_move = game.last_move();
    for row_index in 0..size {
        let row = u16::from(size) - u16::from(row_index);
        let y = m.grid_y0 + u16::from(row_index) * m.row_step;
        for col in 0..size {
            let x = m.grid_x0 + u16::from(col) * m.col_step;
            let estimate = if game.estimate_active {
                overlay
                    .get(row_index as usize)
                    .and_then(|line| line.get(col as usize))
                    .copied()
            } else {
                None
            };
            let is_last = last_move
                .map(|coord| coord.col == col && coord.row == row)
                .unwrap_or(false);

            enum PointVisual {
                Stone(Color),
                DeadStone,
                BlackTerr,
                WhiteTerr,
                Dame,
                Ko,
                Empty,
            }

            let visual = match estimate {
                Some(PointEstimate::DeadStone(_)) => PointVisual::DeadStone,
                Some(PointEstimate::BlackTerritory) => PointVisual::BlackTerr,
                Some(PointEstimate::WhiteTerritory) => PointVisual::WhiteTerr,
                Some(PointEstimate::Dame) => PointVisual::Dame,
                Some(PointEstimate::LiveStone(c)) => PointVisual::Stone(c),
                None => match game.board().get(col, row) {
                    Some(c) => PointVisual::Stone(c),
                    None if game.ko_point() == Some((col, row)) => PointVisual::Ko,
                    None => PointVisual::Empty,
                },
            };

            match visual {
                PointVisual::Empty => {}
                PointVisual::Ko => {
                    let style = Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD);
                    buffer.set_string(x, y, "□", style);
                }
                PointVisual::BlackTerr => {
                    let style = Style::default()
                        .fg(theme.territory_black)
                        .add_modifier(Modifier::BOLD);
                    buffer.set_string(x, y, "■", style);
                }
                PointVisual::WhiteTerr => {
                    let style = Style::default()
                        .fg(theme.territory_white)
                        .add_modifier(Modifier::BOLD);
                    buffer.set_string(x, y, "□", style);
                }
                PointVisual::Dame => {
                    buffer.set_string(x, y, "·", Style::default().fg(theme.muted));
                }
                PointVisual::DeadStone => {
                    let style = Style::default()
                        .fg(theme.dead_stone)
                        .add_modifier(Modifier::DIM);
                    buffer.set_string(x, y, "╳", style);
                }
                PointVisual::Stone(color) => {
                    let base_fg = match color {
                        Color::Black => theme.stone_black,
                        Color::White => theme.stone_white,
                    };
                    let style = Style::default()
                        .fg(if is_last { theme.last_move } else { base_fg })
                        .add_modifier(Modifier::BOLD);

                    let glyph = if is_last {
                        "◉"
                    } else {
                        match color {
                            Color::Black => "●",
                            Color::White => "○",
                        }
                    };
                    buffer.set_string(x, y, glyph, style);
                }
            }
        }
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
    fn intersection_glyphs_have_arms_and_hoshi_crosses() {
        assert_eq!(intersection(9, 4, 4), '╋'); // 9x9 Tengen star point
        assert_eq!(intersection(9, 2, 2), '╋'); // 9x9 corner star point
        assert_eq!(intersection(13, 3, 3), '╋'); // 13x13 star point
        assert_eq!(intersection(19, 9, 9), '╋'); // 19x19 Tengen star point
        assert_eq!(intersection(9, 3, 4), '┼'); // normal interior intersection
        assert_eq!(intersection(9, 0, 0), '┌');
        assert_eq!(intersection(9, 8, 8), '┘');
        assert_eq!(intersection(9, 8, 0), '┐');
        assert_eq!(intersection(9, 0, 8), '└');
    }

    #[test]
    fn text_cell_is_always_two_columns_for_empty_points() {
        for size in [9u8, 13, 19] {
            let board = Board::new(size);
            for col in 0..size {
                for row in 1..=u16::from(size) {
                    let cell = text_cell(&board, size, col, row, None);
                    assert_eq!(cell.chars().count(), 2, "size {size} cell {col},{row}: {cell:?}");
                }
            }
        }
    }

    #[test]
    fn board_steps_and_grid_are_strictly_square_across_all_sizes() {
        for size in [9u8, 13, 19] {
            for &(w, h) in &[
                (60u16, 24u16),
                (80, 30),
                (100, 35),
                (120, 42),
                (160, 50),
                (200, 32),
            ] {
                let area = Rect::new(0, 0, w, h);
                let (col_step, row_step) = compute_board_steps(area, size);
                assert_eq!(
                    col_step,
                    row_step * 2,
                    "size {size} in {w}x{h} must have col_step == row_step * 2 (1:1 visual square)"
                );
                if let Some(m) = compute_metrics(area, size) {
                    assert_eq!(m.grid_w, m.grid_h * 2);
                }
            }
        }
    }

    #[test]
    fn draw_board_renders_circular_stones_not_rectangular_boxes() {
        use crate::ui::tui::theme::ColorSupport;
        let theme = Theme::new(ColorSupport::TrueColor);
        for &(size, w, h) in &[(9u8, 80u16, 40u16), (13, 90, 42), (19, 100, 42)] {
            let mut g = GameView::empty(1, size);
            g.game.play(Coord::new(2, 2)).unwrap();
            g.game.play(Coord::new(3, 2)).unwrap();
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            draw_board(&mut buf, area, &g, &theme, true);
            let rendered: String = buf.content().iter().map(|c| c.symbol()).collect();
            assert!(
                rendered.contains('●') && rendered.contains('◉'),
                "pure circular glyphs expected on {size}x{size}"
            );
            assert!(
                !rendered.contains('▄') && !rendered.contains('▀') && !rendered.contains('█') && !rendered.contains('╭'),
                "no multi-line block art must be used on {size}x{size}"
            );
        }
    }
}
