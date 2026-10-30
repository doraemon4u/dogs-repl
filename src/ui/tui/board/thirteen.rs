//! 13×13: medium density.

use super::BoardStyle;

pub struct Thirteen;

/// 4-4 points and tengen.
const STARS: [(u8, u8); 5] = [(3, 3), (9, 3), (6, 6), (3, 9), (9, 9)];

impl BoardStyle for Thirteen {
    fn size(&self) -> u8 {
        13
    }

    fn star_points(&self) -> &'static [(u8, u8)] {
        &STARS
    }

    fn max_row_step(&self) -> u16 {
        4
    }
}
