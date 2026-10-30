//! 9×9: only 8 gaps, so lines may be far apart and stones are large.

use super::BoardStyle;

pub struct Nine;

/// 3-3 points and tengen.
const STARS: [(u8, u8); 5] = [(2, 2), (6, 2), (4, 4), (2, 6), (6, 6)];

impl BoardStyle for Nine {
    fn size(&self) -> u8 {
        9
    }

    fn star_points(&self) -> &'static [(u8, u8)] {
        &STARS
    }

    fn max_row_step(&self) -> u16 {
        6
    }
}
