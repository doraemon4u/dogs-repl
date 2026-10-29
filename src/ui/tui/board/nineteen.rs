//! 19×19: 361 points; usually one terminal row per line.

use super::BoardStyle;

pub struct Nineteen;

/// The nine standard star points.
const STARS: [(u8, u8); 9] = [
    (3, 3),
    (9, 3),
    (15, 3),
    (3, 9),
    (9, 9),
    (15, 9),
    (3, 15),
    (9, 15),
    (15, 15),
];

impl BoardStyle for Nineteen {
    fn size(&self) -> u8 {
        19
    }

    fn star_points(&self) -> &'static [(u8, u8)] {
        &STARS
    }

    fn max_row_step(&self) -> u16 {
        3
    }
}
