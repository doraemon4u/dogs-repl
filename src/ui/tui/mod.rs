//! TUI submodules.

pub mod app;
pub mod board_render;
pub mod estimate_render;
pub mod input;
pub mod layout;
pub mod theme;
pub mod widgets;

pub use app::{run, Startup};
