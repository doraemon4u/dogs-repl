//! Colour detection and the pane palette.
//!
//! Pane border colours identify *which* module (board / moves / chat); focus is
//! conveyed by brightness and border weight, not by changing the hue (design
//! §9.5). Board content stays greyscale so the two colour systems never mix.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSupport {
    TrueColor,
    Ansi256,
    Ansi16,
}

impl ColorSupport {
    pub fn detect() -> Self {
        Self::from_env(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    pub fn from_env(colorterm: Option<&str>, term: Option<&str>) -> Self {
        let colorterm = colorterm.unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Self::TrueColor;
        }
        if term.unwrap_or_default().contains("256color") {
            return Self::Ansi256;
        }
        Self::Ansi16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub support: ColorSupport,
    pub board: Color,
    pub moves: Color,
    pub chat: Color,
    pub lobby: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub muted: Color,
    pub stone_black: Color,
    pub stone_white: Color,
    pub territory_black: Color,
    pub territory_white: Color,
    pub dead_stone: Color,
    pub last_move: Color,
}

impl Theme {
    pub fn new(support: ColorSupport) -> Self {
        match support {
            ColorSupport::TrueColor => Self {
                support,
                board: Color::Rgb(0, 190, 200),
                moves: Color::Rgb(225, 180, 60),
                chat: Color::Rgb(205, 110, 220),
                lobby: Color::Rgb(90, 140, 240),
                accent: Color::Rgb(245, 245, 245),
                success: Color::Rgb(90, 200, 120),
                warning: Color::Rgb(230, 190, 80),
                error: Color::Rgb(230, 90, 90),
                muted: Color::Rgb(120, 120, 120),
                stone_black: Color::Rgb(88, 175, 255),
                stone_white: Color::Rgb(255, 255, 255),
                territory_black: Color::Rgb(88, 150, 220),
                territory_white: Color::Rgb(210, 210, 210),
                dead_stone: Color::Rgb(210, 100, 100),
                last_move: Color::Rgb(255, 220, 70),
            },
            ColorSupport::Ansi256 => Self {
                support,
                board: Color::Indexed(44),
                moves: Color::Indexed(214),
                chat: Color::Indexed(176),
                lobby: Color::Indexed(69),
                accent: Color::Indexed(255),
                success: Color::Indexed(114),
                warning: Color::Indexed(220),
                error: Color::Indexed(203),
                muted: Color::Indexed(244),
                stone_black: Color::Indexed(75),
                stone_white: Color::Indexed(255),
                territory_black: Color::Indexed(68),
                territory_white: Color::Indexed(252),
                dead_stone: Color::Indexed(174),
                last_move: Color::Indexed(228),
            },
            ColorSupport::Ansi16 => Self {
                support,
                board: Color::Cyan,
                moves: Color::Yellow,
                chat: Color::Magenta,
                lobby: Color::Blue,
                accent: Color::White,
                success: Color::Green,
                warning: Color::Yellow,
                error: Color::Red,
                muted: Color::DarkGray,
                stone_black: Color::Cyan,
                stone_white: Color::White,
                territory_black: Color::Cyan,
                territory_white: Color::White,
                dead_stone: Color::Red,
                last_move: Color::Yellow,
            },
        }
    }

    /// Brightens a pane colour for the focused module.
    pub fn focus(&self, color: Color) -> Color {
        brighten(color, self.support)
    }

    /// Dims a pane colour for unfocused modules.
    pub fn dim(&self, color: Color) -> Color {
        darken(color, self.support)
    }

    pub fn level_color(&self, level: crate::session::store::LogLevel) -> Color {
        use crate::session::store::LogLevel;
        match level {
            LogLevel::Info => self.accent,
            LogLevel::Success => self.success,
            LogLevel::Warning => self.warning,
            LogLevel::Error => self.error,
        }
    }
}

fn brighten(color: Color, support: ColorSupport) -> Color {
    match (color, support) {
        (Color::Rgb(r, g, b), _) => Color::Rgb(
            r.saturating_add(45),
            g.saturating_add(45),
            b.saturating_add(45),
        ),
        (Color::Cyan, _) => Color::LightCyan,
        (Color::Magenta, _) => Color::LightMagenta,
        (Color::Blue, _) => Color::LightBlue,
        (Color::Green, _) => Color::LightGreen,
        (Color::Red, _) => Color::LightRed,
        (Color::Yellow, _) => Color::LightYellow,
        (Color::White, _) => Color::White,
        (Color::DarkGray, _) => Color::Gray,
        (Color::Gray, _) => Color::White,
        (other, _) => other,
    }
}

fn darken(color: Color, support: ColorSupport) -> Color {
    match (color, support) {
        (Color::Rgb(r, g, b), _) => Color::Rgb(
            r.saturating_sub(55),
            g.saturating_sub(55),
            b.saturating_sub(55),
        ),
        (Color::Cyan, _) => Color::DarkGray,
        (Color::Magenta, _) => Color::DarkGray,
        (Color::Blue, _) => Color::DarkGray,
        (Color::Yellow, _) => Color::DarkGray,
        (Color::White, _) => Color::Gray,
        (Color::Gray, _) => Color::DarkGray,
        (other, _) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_truecolor_ansi256_and_fallback() {
        assert_eq!(
            ColorSupport::from_env(Some("truecolor"), Some("xterm")),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env(Some("24bit"), Some("xterm")),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env(None, Some("xterm-256color")),
            ColorSupport::Ansi256
        );
        assert_eq!(
            ColorSupport::from_env(None, Some("xterm")),
            ColorSupport::Ansi16
        );
        assert_eq!(ColorSupport::from_env(None, None), ColorSupport::Ansi16);
    }

    #[test]
    fn theme_levels_are_distinct() {
        for support in [
            ColorSupport::TrueColor,
            ColorSupport::Ansi256,
            ColorSupport::Ansi16,
        ] {
            let theme = Theme::new(support);
            assert_ne!(theme.board, theme.moves);
            assert_ne!(theme.moves, theme.chat);
            assert_ne!(theme.board, theme.chat);
            assert_eq!(theme.support, support);
        }
    }

    #[test]
    fn focus_brightens_and_dim_darkens_rgb() {
        let theme = Theme::new(ColorSupport::TrueColor);
        let focused = theme.focus(theme.board);
        assert_ne!(focused, theme.board);
        let dimmed = theme.dim(theme.board);
        assert_ne!(dimmed, theme.board);
    }

    #[test]
    fn focus_brightens_named_colours() {
        let theme = Theme::new(ColorSupport::Ansi16);
        assert_eq!(theme.focus(theme.board), Color::LightCyan);
        assert_eq!(theme.focus(theme.chat), Color::LightMagenta);
    }
}
