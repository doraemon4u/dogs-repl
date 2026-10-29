//! Presentation layer: command grammar, text REPL, and TUI.

pub mod commands;
pub mod new_game;
pub mod repl;
pub mod tui;

use crate::transport::rest_client::{ChallengeClock, ChallengeOptions};
use commands::{ChallengeClockSpec, ChallengeSpec};

/// A 9×9 local game set up just before a ko: Black takes it at C2, and
/// White's immediate retake at B2 is then illegal.
pub fn preset_ko_command() -> crate::session::Command {
    use crate::engine::coord::Coord;
    let moves = [(1, 3), (2, 3), (0, 2), (1, 2), (1, 1), (3, 2), (6, 6), (2, 1)]
        .into_iter()
        .map(|(col, row)| Coord::new(col, row))
        .collect();
    crate::session::Command::NewLocalGame {
        size: 9,
        rules: crate::engine::rules::RuleSet::Chinese,
        moves,
    }
}

/// Converts the parsed challenge command into the Transport request body.
pub fn challenge_options(spec: ChallengeSpec) -> ChallengeOptions {
    let clock = match spec.clock {
        ChallengeClockSpec::Byoyomi {
            main_time,
            period_time,
            periods,
        } => ChallengeClock::Byoyomi {
            main_time,
            period_time,
            periods,
        },
        ChallengeClockSpec::Fischer {
            initial_time,
            increment,
        } => ChallengeClock::Fischer {
            initial_time,
            increment,
        },
        ChallengeClockSpec::Absolute { main_time } => ChallengeClock::Absolute { main_time },
    };
    ChallengeOptions {
        opponent: spec.opponent,
        board_size: spec.board_size,
        rules: spec.rules,
        ranked: spec.ranked,
        challenger_color: spec.color,
        clock,
        ..ChallengeOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_conversion_preserves_every_option() {
        let spec = ChallengeSpec {
            opponent: "kata".into(),
            board_size: 13,
            rules: "japanese".into(),
            ranked: false,
            color: "black".into(),
            clock: ChallengeClockSpec::Absolute { main_time: 900 },
        };
        let options = challenge_options(spec);
        assert_eq!(options.opponent, "kata");
        assert_eq!(options.board_size, 13);
        assert_eq!(options.rules, "japanese");
        assert!(!options.ranked);
        assert_eq!(options.challenger_color, "black");
        assert_eq!(options.clock, ChallengeClock::Absolute { main_time: 900 });
    }
}
