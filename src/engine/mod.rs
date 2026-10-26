//! Engine layer: pure Go game logic with no I/O dependencies.
//!
//! This module contains the board state machine, coordinate system, rule
//! validation (capture/ko/suicide), SGF read/write, clock state machines, and
//! territory estimation via flood fill.
//!
//! Nothing in this module performs network, filesystem, or terminal I/O, which
//! keeps it independently testable.

pub mod board;
pub mod clock;
pub mod coord;
pub mod estimate;
pub mod game;
pub mod rules;
pub mod sgf;
