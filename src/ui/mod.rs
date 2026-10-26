//! Presentation layer: command grammar, text REPL, and TUI.

pub mod commands;
pub mod repl;
pub mod tui;

use crate::transport::rest_client::{ChallengeClock, ChallengeOptions};
use commands::{ChallengeClockSpec, ChallengeSpec};

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
