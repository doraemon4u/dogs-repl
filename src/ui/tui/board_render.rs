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
pub fn stone_radii(col_step: u16, row_step: u16) -> (u16, u16) {
    let rx = if col_step >= 5 {
        2
    } else if col_step >= 3 {
        1
    } else {
        0
    };
    let ry = if row_step >= 2 && col_step >= 3 {
        1
    } else {
        0
    };
    (rx, ry)
}

/// Computes optimal `(col_step, row_step)` so 9x9, 13x13, and 19x19 boards all expand
/// to fill the 2/3 board pane and render proportional multi-character stones.
pub fn compute_board_steps(area: Rect, size: u8) -> (u16, u16) {
    if size <= 1 {
        return (2, 1);
    }
    let gaps = u16::from(size - 1);

    // Find maximum horizontal step that fits `area.width`
    let mut max_col = 2u16;
    for c in 2..=12 {
        let rx = if c >= 5 { 2 } else if c >= 3 { 1 } else { 0 };
        let needed_w = LABEL_WIDTH + rx + gaps * c + rx + 1;
        if needed_w <= area.width {
            max_col = c;
        } else {
            break;
        }
    }

    // Find maximum vertical step that fits `area.height`
    let mut max_row = 1u16;
    for r in 1..=6 {
        let needed_h = if r >= 3 {
            gaps * r + 4
        } else if r == 2 {
            // Allow 19x19 (gaps=18 -> 36+1=37) to enter 2D stone mode in standard full-screen terminals
            gaps * r + 1
        } else {
            gaps * r + 2
        };
        let min_col_for_r = if r == 2 { 3 } else { r * 2 };
        if needed_h <= area.height && max_col >= min_col_for_r {
            max_row = r;
        } else {
            break;
        }
    }

    let row_step = max_row.max(1);
    let col_step = if row_step == 1 {
        // When vertical height limits row_step to 1 (e.g. 19x19 on shorter terminals),
        // still expand horizontally up to 5 cols per step so 19x19 fills the pane and uses 3..5-col stones.
        max_col.clamp(2, 5)
    } else if row_step == 2 {
        // For row_step == 2 (19x19 or 13x13), expand up to 6 cols per step so 5x3 stones can be used on wide screens
        max_col.clamp(3, 6)
    } else {
        // For row_step >= 3 (9x9 or 13x13), keep ~2:1 aspect ratio
        (row_step * 2 + 2).min(max_col).max(row_step * 2).min(max_col)
    };

    (col_step, row_step)
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
    let min_draw_y = if m.show_top_coords {
        m.origin_y + 1
    } else {
        area.y
    };
    let max_draw_y = if m.show_bottom_coords {
        (m.grid_y0 + m.grid_h + m.stone_ry + 1).min(area.y + area.height)
    } else {
        area.y + area.height
    };

    let has_visual_stone = |c: u8, r_idx: u8| -> bool {
        if r_idx >= size || c >= size {
            return false;
        }
        let r = u16::from(size) - u16::from(r_idx);
        if game.estimate_active {
            let est = overlay
                .get(r_idx as usize)
                .and_then(|line| line.get(c as usize))
                .copied();
            matches!(
                est,
                Some(PointEstimate::LiveStone(_)) | Some(PointEstimate::DeadStone(_))
            )
        } else {
            game.board().get(c, r).is_some()
        }
    };

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
                    if m.stone_rx >= 1 {
                        buffer.set_string(x - 1, y, "[◆]", style);
                    } else {
                        buffer.set_string(x, y, "◆", style);
                    }
                }
                PointVisual::BlackTerr => {
                    let style = Style::default()
                        .fg(theme.territory_black)
                        .add_modifier(Modifier::BOLD);
                    if m.stone_rx >= 1 {
                        buffer.set_string(x - 1, y, " ■ ", style);
                    } else {
                        buffer.set_string(x, y, "■", style);
                    }
                }
                PointVisual::WhiteTerr => {
                    let style = Style::default()
                        .fg(theme.territory_white)
                        .add_modifier(Modifier::BOLD);
                    if m.stone_rx >= 1 {
                        buffer.set_string(x - 1, y, " □ ", style);
                    } else {
                        buffer.set_string(x, y, "□", style);
                    }
                }
                PointVisual::Dame => {
                    buffer.set_string(x, y, "·", Style::default().fg(theme.muted));
                }
                PointVisual::DeadStone => {
                    let style = Style::default()
                        .fg(theme.dead_stone)
                        .add_modifier(Modifier::DIM);
                    if m.stone_rx == 2 && m.stone_ry == 1 {
                        if y > min_draw_y {
                            buffer.set_string(x - 2, y - 1, "╭───╮", style);
                        }
                        buffer.set_string(x - 2, y, "│ ╳ │", style);
                        if y + 1 < max_draw_y {
                            buffer.set_string(x - 2, y + 1, "╰───╯", style);
                        }
                    } else if m.stone_rx == 1 && m.stone_ry == 1 {
                        if y > min_draw_y {
                            buffer.set_string(x - 1, y - 1, "╭─╮", style);
                        }
                        buffer.set_string(x - 1, y, "│╳│", style);
                        if y + 1 < max_draw_y {
                            buffer.set_string(x - 1, y + 1, "╰─╯", style);
                        }
                    } else if m.stone_rx >= 1 {
                        buffer.set_string(x - 1, y, "(╳)", style);
                    } else {
                        buffer.set_string(x, y, "╳", style);
                    }
                }
                PointVisual::Stone(color) => {
                    let base_fg = match color {
                        Color::Black => theme.stone_black,
                        Color::White => theme.stone_white,
                    };
                    let border_style = Style::default()
                        .fg(if is_last { theme.last_move } else { base_fg })
                        .add_modifier(Modifier::BOLD);
                    let body_style = Style::default()
                        .fg(base_fg)
                        .add_modifier(Modifier::BOLD);
                    let last_center_style = Style::default()
                        .fg(theme.last_move)
                        .add_modifier(Modifier::BOLD);

                    let neighbor_up = row_index > 0
                        && m.row_step == 2
                        && has_visual_stone(col, row_index - 1);
                    let neighbor_down = row_index + 1 < size
                        && m.row_step == 2
                        && has_visual_stone(col, row_index + 1);

                    if m.stone_rx == 2 && m.stone_ry == 1 {
                        // 5x3 Large Stone
                        if y >= min_draw_y + 1 {
                            let top_str = if neighbor_up { "├───┤" } else { "╭───╮" };
                            buffer.set_string(x - 2, y - 1, top_str, border_style);
                        }
                        let mid_str = match color {
                            Color::Black => "│███│",
                            Color::White => "│ ○ │",
                        };
                        buffer.set_string(x - 2, y, mid_str, body_style);
                        if is_last {
                            buffer.set_string(x - 2, y, "│", border_style);
                            buffer.set_string(x + 2, y, "│", border_style);
                            buffer.set_string(x, y, "◉", last_center_style);
                        }
                        if y + 1 < max_draw_y {
                            let bot_str = if neighbor_down { "├───┤" } else { "╰───╯" };
                            buffer.set_string(x - 2, y + 1, bot_str, border_style);
                        }
                    } else if m.stone_rx == 1 && m.stone_ry == 1 {
                        // 3x3 Medium Stone
                        if y >= min_draw_y + 1 {
                            let top_str = if neighbor_up { "├─┤" } else { "╭─╮" };
                            buffer.set_string(x - 1, y - 1, top_str, border_style);
                        }
                        let mid_str = match color {
                            Color::Black => "│█│",
                            Color::White => "│○│",
                        };
                        buffer.set_string(x - 1, y, mid_str, body_style);
                        if is_last {
                            buffer.set_string(x - 1, y, "│", border_style);
                            buffer.set_string(x + 1, y, "│", border_style);
                            buffer.set_string(x, y, "◉", last_center_style);
                        }
                        if y + 1 < max_draw_y {
                            let bot_str = if neighbor_down { "├─┤" } else { "╰─╯" };
                            buffer.set_string(x - 1, y + 1, bot_str, border_style);
                        }
                    } else if m.stone_rx >= 1 {
                        // 3x1 Wide Stone (e.g. 19x19 in standard-height terminals)
                        let glyph = match color {
                            Color::Black => "(█)",
                            Color::White => "(○)",
                        };
                        buffer.set_string(x - 1, y, glyph, body_style);
                        if is_last {
                            buffer.set_string(x - 1, y, "(", border_style);
                            buffer.set_string(x + 1, y, ")", border_style);
                            buffer.set_string(x, y, "◉", last_center_style);
                        }
                    } else {
                        // 1x1 Compact Stone
                        let glyph = match color {
                            Color::Black => "●",
                            Color::White => "○",
                        };
                        let st = if is_last {
                            body_style.fg(theme.last_move).add_modifier(Modifier::REVERSED)
                        } else {
                            body_style
                        };
                        buffer.set_string(x, y, glyph, st);
                    }
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
}
