//! Per-size board styles and the shared stone painter.
//!
//! Each supported size has its own module with its own star points and
//! spacing limit. Stones follow one rule on every size: a stone is centred on
//! its intersection and is exactly one grid cell across (from the midpoint to
//! the previous line to the midpoint to the next), so neighbouring stones
//! touch but never overlap. To get that exactly in a terminal, stones are
//! painted at quarter-character resolution with quadrant block glyphs.

mod nine;
mod nineteen;
mod thirteen;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Everything that differs between board sizes.
pub trait BoardStyle: Sync {
    /// Lines on the board (9, 13, or 19).
    #[allow(dead_code)] // read by tests to check the size → style mapping
    fn size(&self) -> u8;

    /// Star points (星位) as `(col, row_index)`, row 0 at the top.
    fn star_points(&self) -> &'static [(u8, u8)];

    /// Largest vertical step between lines. Bigger boards stop growing
    /// earlier so the grid stays readable.
    fn max_row_step(&self) -> u16;

    fn is_star(&self, col: u8, row_index: u8) -> bool {
        self.star_points().contains(&(col, row_index))
    }
}

/// The style for `size`, or `None` for sizes the client cannot create.
pub fn style_for(size: u8) -> Option<&'static dyn BoardStyle> {
    match size {
        9 => Some(&nine::Nine),
        13 => Some(&thirteen::Thirteen),
        19 => Some(&nineteen::Nineteen),
        _ => None,
    }
}

/// The style for `size`, falling back to the closest supported size for
/// boards that only arrive from SGF files or OGS (e.g. 7×7 or 21×21).
pub fn style_or_nearest(size: u8) -> &'static dyn BoardStyle {
    style_for(size).unwrap_or(match size {
        0..=10 => &nine::Nine,
        11..=15 => &thirteen::Thirteen,
        _ => &nineteen::Nineteen,
    })
}

/// How many character cells a stone reaches past its intersection,
/// horizontally and vertically: half a grid cell, rounded to whole cells.
pub const fn stone_extent(col_step: u16, row_step: u16) -> (u16, u16) {
    (col_step / 2, row_step / 2)
}

/// Quadrant glyphs indexed by mask: 1 upper-left, 2 upper-right,
/// 4 lower-left, 8 lower-right.
const QUADRANTS: [&str; 16] = [
    " ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█",
];

/// Collects stone discs at quarter-cell resolution, then writes them into a
/// buffer, merging stones that share a character cell.
pub struct StoneCanvas {
    area: Rect,
    /// Colour per sub-cell; two sub-cells per character in each direction.
    pixels: Vec<Option<Color>>,
}

impl StoneCanvas {
    pub fn new(area: Rect) -> Self {
        let len = usize::from(area.width) * 2 * usize::from(area.height) * 2;
        Self {
            area,
            pixels: vec![None; len],
        }
    }

    fn sub_width(&self) -> i32 {
        i32::from(self.area.width) * 2
    }

    fn sub_height(&self) -> i32 {
        i32::from(self.area.height) * 2
    }

    /// Paints a disc centred on intersection `(x, y)` (absolute terminal
    /// cell) whose bounding box is exactly one grid cell:
    /// `col_step` columns wide and `row_step` rows tall.
    pub fn paint_stone(&mut self, x: u16, y: u16, col_step: u16, row_step: u16, color: Color) {
        // Sub-cell units: an intersection sits at the centre of its cell.
        let cx = (i32::from(x) - i32::from(self.area.x)) * 2 + 1;
        let cy = (i32::from(y) - i32::from(self.area.y)) * 2 + 1;
        // Half the cell, in sub-cells.
        let rx = f64::from(col_step.max(1));
        let ry = f64::from(row_step.max(1));
        let reach_x = i32::from(col_step.max(1));
        let reach_y = i32::from(row_step.max(1));
        for sy in (cy - reach_y)..(cy + reach_y) {
            if sy < 0 || sy >= self.sub_height() {
                continue;
            }
            for sx in (cx - reach_x)..(cx + reach_x) {
                if sx < 0 || sx >= self.sub_width() {
                    continue;
                }
                // Distance from the sub-cell's centre to the intersection.
                let dx = (f64::from(sx) + 0.5 - f64::from(cx)) / rx;
                let dy = (f64::from(sy) + 0.5 - f64::from(cy)) / ry;
                if dx * dx + dy * dy <= 1.0 + 1e-9 {
                    let index = (sy * self.sub_width() + sx) as usize;
                    self.pixels[index] = Some(color);
                }
            }
        }
    }

    /// The colour painted at a sub-cell, for tests.
    #[cfg(test)]
    pub fn pixel(&self, sub_x: i32, sub_y: i32) -> Option<Color> {
        self.pixels[(sub_y * self.sub_width() + sub_x) as usize]
    }

    /// Writes every touched cell into `buffer`. A cell shared by two
    /// stones gets one colour as foreground and the other as background.
    pub fn flush(&self, buffer: &mut Buffer) {
        for row in 0..self.area.height {
            for col in 0..self.area.width {
                let (sx, sy) = (i32::from(col) * 2, i32::from(row) * 2);
                let quads = [
                    self.pixels[(sy * self.sub_width() + sx) as usize],
                    self.pixels[(sy * self.sub_width() + sx + 1) as usize],
                    self.pixels[((sy + 1) * self.sub_width() + sx) as usize],
                    self.pixels[((sy + 1) * self.sub_width() + sx + 1) as usize],
                ];
                let Some(first) = quads.iter().flatten().next().copied() else {
                    continue;
                };
                // The colour covering most of the cell is the foreground.
                let count = |c: Color| quads.iter().filter(|q| **q == Some(c)).count();
                let other = quads.iter().flatten().copied().find(|c| *c != first);
                let (fg, bg) = match other {
                    Some(o) if count(o) > count(first) => (o, Some(first)),
                    Some(o) => (first, Some(o)),
                    None => (first, None),
                };
                let mask = quads
                    .iter()
                    .enumerate()
                    .filter(|(_, q)| **q == Some(fg))
                    .fold(0usize, |mask, (bit, _)| mask | (1 << bit));
                let mut style = Style::default().fg(fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                let x = self.area.x + col;
                let y = self.area.y + row;
                buffer.set_string(x, y, QUADRANTS[mask], style);
            }
        }
    }
}

/// Marks the centre of a stone (last move, dead) without changing its size.
pub fn mark_centre(buffer: &mut Buffer, area: Rect, x: u16, y: u16, glyph: &str, fg: Color, bg: Color) {
    if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
        buffer.set_string(
            x,
            y,
            glyph,
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_size_has_its_own_style() {
        for size in crate::engine::rules::SUPPORTED_SIZES {
            assert_eq!(style_for(size).map(|style| style.size()), Some(size));
        }
        assert!(style_for(11).is_none());
        assert_eq!(style_or_nearest(7).size(), 9);
        assert_eq!(style_or_nearest(21).size(), 19);
    }

    #[test]
    fn star_points_lie_on_the_board() {
        for size in crate::engine::rules::SUPPORTED_SIZES {
            let style = style_for(size).unwrap();
            for &(col, row) in style.star_points() {
                assert!(col < size && row < size);
            }
        }
    }

    /// Bounding box of everything painted, in sub-cells.
    fn bounds(canvas: &StoneCanvas) -> (i32, i32, i32, i32) {
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for sy in 0..canvas.sub_height() {
            for sx in 0..canvas.sub_width() {
                if canvas.pixel(sx, sy).is_some() {
                    x0 = x0.min(sx);
                    y0 = y0.min(sy);
                    x1 = x1.max(sx);
                    y1 = y1.max(sy);
                }
            }
        }
        (x0, y0, x1, y1)
    }

    #[test]
    fn a_stone_is_exactly_one_grid_cell_across_and_centred() {
        for row_step in 1..=6u16 {
            let col_step = row_step * 2;
            let area = Rect::new(0, 0, 60, 30);
            let mut canvas = StoneCanvas::new(area);
            let (x, y) = (20u16, 12u16);
            canvas.paint_stone(x, y, col_step, row_step, Color::Black);
            let (x0, y0, x1, y1) = bounds(&canvas);
            // Width and height in sub-cells = one grid cell (2 per character).
            assert_eq!(x1 - x0 + 1, i32::from(col_step) * 2, "width at step {row_step}");
            assert_eq!(y1 - y0 + 1, i32::from(row_step) * 2, "height at step {row_step}");
            // Centred on the intersection's centre.
            let (cx, cy) = (i32::from(x) * 2 + 1, i32::from(y) * 2 + 1);
            assert_eq!(cx - x0, x1 + 1 - cx);
            assert_eq!(cy - y0, y1 + 1 - cy);
            // The extent used for padding matches what is painted.
            let (ex, ey) = stone_extent(col_step, row_step);
            assert_eq!(i32::from(x) - ex as i32, x0 / 2);
            assert_eq!(i32::from(y) - ey as i32, y0 / 2);
        }
    }

    #[test]
    fn neighbouring_stones_touch_at_the_midpoint_without_overlapping() {
        for row_step in 1..=4u16 {
            let col_step = row_step * 2;
            let area = Rect::new(0, 0, 60, 30);
            let mut canvas = StoneCanvas::new(area);
            let (x, y) = (10u16, 10u16);
            canvas.paint_stone(x, y, col_step, row_step, Color::Black);
            canvas.paint_stone(x + col_step, y, col_step, row_step, Color::White);
            canvas.paint_stone(x, y + row_step, col_step, row_step, Color::White);
            // Along the shared edge: the left half is black, the right half white.
            let mid = (i32::from(x) * 2 + 1) + i32::from(col_step);
            let row = i32::from(y) * 2;
            assert_eq!(canvas.pixel(mid - 1, row), Some(Color::Black));
            assert_eq!(canvas.pixel(mid, row), Some(Color::White));
            let col = i32::from(x) * 2;
            let mid_y = (i32::from(y) * 2 + 1) + i32::from(row_step);
            assert_eq!(canvas.pixel(col, mid_y - 1), Some(Color::Black));
            assert_eq!(canvas.pixel(col, mid_y), Some(Color::White));
        }
    }

    #[test]
    fn shared_cells_blend_both_colours() {
        let area = Rect::new(0, 0, 10, 3);
        let mut canvas = StoneCanvas::new(area);
        // 19×19 spacing: two columns per line, one row.
        canvas.paint_stone(2, 1, 2, 1, Color::Black);
        canvas.paint_stone(4, 1, 2, 1, Color::White);
        let mut buffer = Buffer::empty(area);
        canvas.flush(&mut buffer);
        assert_eq!(buffer[(2, 1)].symbol(), "█");
        assert_eq!(buffer[(4, 1)].symbol(), "█");
        // The cell between them is half black (left), half white (right).
        let shared = &buffer[(3, 1)];
        assert!(matches!(shared.symbol(), "▌" | "▐"), "{}", shared.symbol());
        assert_ne!(shared.fg, shared.bg);
        assert_eq!(buffer[(1, 1)].symbol(), "▐", "outer half of the black stone");
        assert_eq!(buffer[(5, 1)].symbol(), "▌", "outer half of the white stone");
        assert_eq!(buffer[(0, 1)].symbol(), " ");
    }
}
