//! Territory-estimate rendering (design §10.3).
//!
//! This reuses the ordinary board pipeline and swaps the data source for the
//! `PointEstimate` overlay. Board content stays greyscale; the pane border
//! palette identifies the module.

use crate::session::store::GameView;
use crate::ui::tui::board_render;

/// Plain-text estimate view used by the REPL and by snapshot tests.
pub fn render_text(game: &GameView) -> String {
    let overlay = game.estimate_overlay();
    let mut out = board_render::render_text_with_overlay(game, Some(&overlay));
    out.push_str("\n\nESTIMATE MODE — local only (not the official OGS result)");
    if let Some(summary) = &game.estimate_summary {
        out.push('\n');
        out.push_str(&format!("{summary}"));
        out.push_str(&format!(
            "\nB territory {} · W territory {} · dame {} · dead B/W {}/{}",
            summary.black_territory,
            summary.white_territory,
            summary.dame,
            summary.dead_black,
            summary.dead_white
        ));
    }
    out.push_str(
        "\nLegend: ■ black territory, □ white territory, · dame, ╳ dead stone\
         \n[a10] toggles the connected group · `estimate confirm` recounts · `estimate exit` leaves",
    );
    out
}

/// One-line estimate status for a pane title or status bar.
pub fn status_line(game: &GameView) -> Option<String> {
    game.estimate_summary.as_ref().map(|summary| {
        format!(
            "estimate: {} · dead {} · local only",
            summary.result(),
            game.estimate_dead.len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::coord::Coord;

    #[test]
    fn estimate_text_contains_legend_and_summary() {
        let mut game = GameView::empty(1, 9);
        game.enter_estimate();
        game.toggle_estimate_dead(Coord::new(2, 2));
        game.toggle_estimate_dead(Coord::new(6, 6));
        let text = render_text(&game);
        assert!(text.contains("ESTIMATE MODE"));
        assert!(text.contains("Legend"));
        assert!(text.contains("local only"));
        assert!(status_line(&game).unwrap().contains("estimate"));
    }

    #[test]
    fn status_line_is_none_outside_estimate_mode() {
        let game = GameView::empty(1, 9);
        assert!(status_line(&game).is_none());
    }
}
