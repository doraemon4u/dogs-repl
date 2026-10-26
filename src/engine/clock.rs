//! Time-control state machine: absolute, byo-yomi, Fischer, Canadian.
//!
//! The clock is pure Engine state. It is advanced by an explicit elapsed
//! `Duration` (driven by the Session tick), so it is fully testable without a
//! wall clock or a network connection.

use super::board::Color;
use std::time::Duration;

/// Supported time controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeControl {
    /// No clock (correspondence or analysis).
    None,
    /// One lump of main time; flagging loses.
    Absolute { main: Duration },
    /// Japanese-style overtime: main time plus N periods of `period` each.
    Byoyomi {
        main: Duration,
        periods: u32,
        period: Duration,
    },
    /// Fischer: main time plus an increment added after every move.
    Fischer { main: Duration, increment: Duration },
    /// Canadian: main time, then `stones` moves within `period`.
    Canadian {
        main: Duration,
        stones: u32,
        period: Duration,
    },
}

impl TimeControl {
    /// Main time configured for both players.
    pub const fn main(&self) -> Duration {
        match self {
            Self::None => Duration::ZERO,
            Self::Absolute { main }
            | Self::Byoyomi { main, .. }
            | Self::Fischer { main, .. }
            | Self::Canadian { main, .. } => *main,
        }
    }

    /// Fischer increment, if any.
    pub const fn increment(&self) -> Option<Duration> {
        match self {
            Self::Fischer { increment, .. } => Some(*increment),
            _ => None,
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Serializes this time control to standard SGF `TM` (seconds) and `OT`
    /// (overtime description) fields.
    pub fn to_sgf_fields(&self) -> (Option<u32>, Option<String>) {
        match self {
            Self::None => (None, None),
            Self::Absolute { main } => (Some(main.as_secs() as u32), Some("none".to_string())),
            Self::Byoyomi {
                main,
                periods,
                period,
            } => (
                Some(main.as_secs() as u32),
                Some(format!("{}x{} byo-yomi", periods, period.as_secs())),
            ),
            Self::Fischer { main, increment } => (
                Some(main.as_secs() as u32),
                Some(format!("{} fischer", increment.as_secs())),
            ),
            Self::Canadian {
                main,
                stones,
                period,
            } => (
                Some(main.as_secs() as u32),
                Some(format!("{}/{} canadian", stones, period.as_secs())),
            ),
        }
    }

    /// Parses SGF `TM` (main time in seconds) and `OT` (overtime string) into a
    /// [`TimeControl`].
    pub fn from_sgf_fields(main_secs: Option<u32>, overtime: Option<&str>) -> Self {
        let main = Duration::from_secs(u64::from(main_secs.unwrap_or(0)));
        let Some(ot_raw) = overtime.map(str::trim).filter(|s| !s.is_empty()) else {
            return if main.is_zero() {
                Self::None
            } else {
                Self::Absolute { main }
            };
        };
        let lower = ot_raw.to_ascii_lowercase();
        if lower == "none" || lower == "-" {
            return if main.is_zero() {
                Self::None
            } else {
                Self::Absolute { main }
            };
        }
        if lower.contains("byo") {
            // e.g. "5x30 byo-yomi" or "5x30s byo-yomi"
            let token = lower.split_whitespace().next().unwrap_or_default();
            if let Some((p_str, t_str)) = token.split_once('x') {
                let periods = p_str.trim().parse::<u32>().unwrap_or(5).max(1);
                let secs = t_str
                    .trim_end_matches('s')
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(30);
                return Self::Byoyomi {
                    main,
                    periods,
                    period: Duration::from_secs(secs),
                };
            }
        }
        if lower.contains("canadian") || lower.contains('/') {
            // e.g. "25/600 canadian"
            let token = lower.split_whitespace().next().unwrap_or_default();
            if let Some((s_str, t_str)) = token.split_once('/') {
                let stones = s_str.trim().parse::<u32>().unwrap_or(25).max(1);
                let secs = t_str
                    .trim_end_matches('s')
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(300);
                return Self::Canadian {
                    main,
                    stones,
                    period: Duration::from_secs(secs),
                };
            }
        }
        if lower.contains("fischer") || lower.contains('+') {
            let num_token = lower
                .split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(5);
            return Self::Fischer {
                main,
                increment: Duration::from_secs(num_token),
            };
        }
        if main.is_zero() {
            Self::None
        } else {
            Self::Absolute { main }
        }
    }
}

/// One player's clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerClock {
    /// Remaining main time.
    pub main: Duration,
    /// Remaining periods (byo-yomi) or moves (Canadian).
    pub periods_left: u32,
    /// Time left in the current overtime period.
    pub period_time: Duration,
    /// Moves left in the current Canadian period.
    pub stones_left: u32,
}

impl PlayerClock {
    fn from_control(control: &TimeControl) -> Self {
        let (periods_left, period_time, stones_left) = match control {
            TimeControl::Byoyomi {
                periods, period, ..
            } => (*periods, *period, 0),
            TimeControl::Canadian { stones, period, .. } => (0, *period, *stones),
            _ => (0, Duration::ZERO, 0),
        };
        Self {
            main: control.main(),
            periods_left,
            period_time,
            stones_left,
        }
    }

    /// Whether this player is now playing in overtime.
    pub fn in_overtime(&self) -> bool {
        self.main.is_zero() && !self.period_time.is_zero()
    }

    /// Human-readable main/overtime text, e.g. `12:34` or `3×30s`.
    pub fn display(&self, control: &TimeControl) -> String {
        if !self.main.is_zero() || self.period_time.is_zero() {
            return format_duration(self.main);
        }
        match control {
            TimeControl::Byoyomi { .. } => format!(
                "{}×{}",
                self.periods_left,
                format_duration(self.period_time)
            ),
            TimeControl::Canadian { .. } => {
                format!(
                    "{} stones / {}",
                    self.stones_left,
                    format_duration(self.period_time)
                )
            }
            _ => format_duration(self.period_time),
        }
    }
}

/// Both clocks plus whose turn it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clock {
    control: TimeControl,
    black: PlayerClock,
    white: PlayerClock,
    active: Option<Color>,
    flagged: Option<Color>,
}

/// Outcome of advancing the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// Time was decremented and nobody flagged.
    Running,
    /// The active player ran out of time.
    Flagged(Color),
    /// The clock is stopped.
    Stopped,
}

impl Clock {
    /// Creates a clock at `main` for both players, with `active` to move.
    pub fn new(control: TimeControl, active: Option<Color>) -> Self {
        Self {
            black: PlayerClock::from_control(&control),
            white: PlayerClock::from_control(&control),
            active,
            flagged: None,
            control,
        }
    }

    pub fn control(&self) -> &TimeControl {
        &self.control
    }

    pub const fn active(&self) -> Option<Color> {
        self.active
    }

    pub const fn flagged(&self) -> Option<Color> {
        self.flagged
    }

    pub fn player(&self, color: Color) -> &PlayerClock {
        match color {
            Color::Black => &self.black,
            Color::White => &self.white,
        }
    }

    fn player_mut(&mut self, color: Color) -> &mut PlayerClock {
        match color {
            Color::Black => &mut self.black,
            Color::White => &mut self.white,
        }
    }

    /// Starts or stops the clock. An already-flagged clock ignores this.
    pub fn set_active(&mut self, active: Option<Color>) {
        if self.flagged.is_none() {
            self.active = active;
        }
    }

    /// Advances the active player's clock by `elapsed`.
    pub fn tick(&mut self, elapsed: Duration) -> TickOutcome {
        let Some(active) = self.active else {
            return TickOutcome::Stopped;
        };
        if self.flagged.is_some() {
            return TickOutcome::Stopped;
        }

        let control = self.control.clone();
        let player = self.player_mut(active);
        let mut left = elapsed;

        loop {
            if !player.main.is_zero() {
                if left < player.main {
                    player.main -= left;
                    return TickOutcome::Running;
                }
                left -= player.main;
                player.main = Duration::ZERO;
                match &control {
                    // Main time running out starts overtime with a full unit.
                    TimeControl::Byoyomi { .. } | TimeControl::Canadian { .. } => {
                        if left.is_zero() {
                            return TickOutcome::Running;
                        }
                        continue;
                    }
                    TimeControl::None => return TickOutcome::Stopped,
                    // Absolute and Fischer clock: zero main time is a loss.
                    TimeControl::Absolute { .. } | TimeControl::Fischer { .. } => {
                        self.flagged = Some(active);
                        self.active = None;
                        return TickOutcome::Flagged(active);
                    }
                }
            }

            if !player.period_time.is_zero() {
                if left < player.period_time {
                    player.period_time -= left;
                    return TickOutcome::Running;
                }
                left -= player.period_time;
                player.period_time = Duration::ZERO;
            }

            // The current overtime unit is exhausted. Consuming it even when
            // `left` is exactly zero is what makes an exactly-used period
            // register as consumed rather than silently rolling over.
            match &control {
                TimeControl::Byoyomi { period, .. } => {
                    if player.periods_left > 1 {
                        player.periods_left -= 1;
                        player.period_time = *period;
                        if player.period_time.is_zero() {
                            player.periods_left = 1;
                        }
                        continue;
                    }
                    self.flagged = Some(active);
                    self.active = None;
                    return TickOutcome::Flagged(active);
                }
                TimeControl::Canadian { .. } => {
                    // Canadian overtime has a single period per `stones` moves;
                    // running out before those moves are played loses.
                    self.flagged = Some(active);
                    self.active = None;
                    return TickOutcome::Flagged(active);
                }
                TimeControl::Absolute { .. } | TimeControl::Fischer { .. } => {
                    self.flagged = Some(active);
                    self.active = None;
                    return TickOutcome::Flagged(active);
                }
                TimeControl::None => return TickOutcome::Stopped,
            }
        }
    }

    /// Records that `color` played, applies overtime/increment rules, and
    /// hands the turn to the opponent.
    pub fn on_move(&mut self, color: Color) {
        if self.flagged.is_some() {
            return;
        }
        match self.control.clone() {
            TimeControl::Fischer { increment, .. } => {
                let player = self.player_mut(color);
                player.main += increment;
            }
            TimeControl::Byoyomi { period, .. } => {
                let player = self.player_mut(color);
                if player.main.is_zero() {
                    // A move in a byo-yomi period restarts that period; the
                    // number of remaining periods is unchanged.
                    player.period_time = period;
                }
            }
            TimeControl::Canadian { stones, period, .. } => {
                let player = self.player_mut(color);
                if player.main.is_zero() {
                    if player.stones_left <= 1 {
                        player.stones_left = stones;
                        player.period_time = period;
                    } else {
                        player.stones_left -= 1;
                    }
                }
            }
            TimeControl::Absolute { .. } | TimeControl::None => {}
        }
        self.active = Some(color.opponent());
    }

    /// Replaces the active player's remaining main time from a server clock
    /// update. Negative values are clamped to zero.
    pub fn set_main(&mut self, color: Color, seconds: f64) {
        let seconds = seconds.max(0.0);
        self.player_mut(color).main = Duration::from_secs_f64(seconds);
    }

    /// Synchronizes main and overtime clock fields from a server clock object.
    pub fn sync_player(
        &mut self,
        color: Color,
        main_seconds: Option<f64>,
        periods_left: Option<u32>,
        period_time_seconds: Option<f64>,
        stones_left: Option<u32>,
    ) {
        let player = self.player_mut(color);
        if let Some(seconds) = main_seconds {
            player.main = Duration::from_secs_f64(seconds.max(0.0));
        }
        if let Some(periods) = periods_left {
            player.periods_left = periods;
        }
        if let Some(period_secs) = period_time_seconds {
            player.period_time = Duration::from_secs_f64(period_secs.max(0.0));
        }
        if let Some(stones) = stones_left {
            player.stones_left = stones;
        }
    }
}

/// Formats a duration as `MM:SS`, or `H:MM:SS` when it is at least an hour.
pub fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn absolute_flags_when_main_time_runs_out() {
        let mut clock = Clock::new(
            TimeControl::Absolute { main: secs(10) },
            Some(Color::Black),
        );
        assert_eq!(clock.tick(secs(4)), TickOutcome::Running);
        assert_eq!(clock.player(Color::Black).main, secs(6));
        assert_eq!(clock.tick(secs(7)), TickOutcome::Flagged(Color::Black));
        assert_eq!(clock.flagged(), Some(Color::Black));
        assert_eq!(clock.active(), None);
        // Further ticks stop.
        assert_eq!(clock.tick(secs(1)), TickOutcome::Stopped);
    }

    #[test]
    fn stopped_clock_does_not_tick() {
        let mut clock = Clock::new(TimeControl::None, None);
        assert_eq!(clock.tick(secs(100)), TickOutcome::Stopped);
    }

    #[test]
    fn byoyomi_main_then_periods() {
        let mut clock = Clock::new(
            TimeControl::Byoyomi {
                main: secs(5),
                periods: 2,
                period: secs(10),
            },
            Some(Color::White),
        );
        assert_eq!(clock.tick(secs(5)), TickOutcome::Running);
        assert_eq!(clock.player(Color::White).main, Duration::ZERO);
        assert_eq!(clock.player(Color::White).period_time, secs(10));
        // Use up the first period.
        assert_eq!(clock.tick(secs(10)), TickOutcome::Running);
        assert_eq!(clock.player(Color::White).periods_left, 1);
        assert_eq!(clock.player(Color::White).period_time, secs(10));
        // Use up the last period.
        assert_eq!(clock.tick(secs(10)), TickOutcome::Flagged(Color::White));
    }

    #[test]
    fn byoyomi_move_resets_the_period() {
        let mut clock = Clock::new(
            TimeControl::Byoyomi {
                main: secs(1),
                periods: 1,
                period: secs(20),
            },
            Some(Color::Black),
        );
        clock.tick(secs(1));
        clock.tick(secs(15));
        assert_eq!(clock.player(Color::Black).period_time, secs(5));
        clock.on_move(Color::Black);
        assert_eq!(clock.player(Color::Black).period_time, secs(20));
        assert_eq!(clock.active(), Some(Color::White));
    }

    #[test]
    fn fischer_adds_increment_after_the_move() {
        let mut clock = Clock::new(
            TimeControl::Fischer {
                main: secs(60),
                increment: secs(10),
            },
            Some(Color::Black),
        );
        clock.tick(secs(30));
        assert_eq!(clock.player(Color::Black).main, secs(30));
        clock.on_move(Color::Black);
        assert_eq!(clock.player(Color::Black).main, secs(40));
        assert_eq!(clock.active(), Some(Color::White));
    }

    #[test]
    fn canadian_resets_after_the_stone_count() {
        let mut clock = Clock::new(
            TimeControl::Canadian {
                main: secs(1),
                stones: 2,
                period: secs(30),
            },
            Some(Color::Black),
        );
        clock.tick(secs(1));
        clock.on_move(Color::Black);
        assert_eq!(clock.player(Color::Black).stones_left, 1);
        // White's turn; give Black the turn back.
        clock.set_active(Some(Color::Black));
        clock.on_move(Color::Black);
        assert_eq!(clock.player(Color::Black).stones_left, 2);
        assert_eq!(clock.player(Color::Black).period_time, secs(30));
    }

    #[test]
    fn canadian_out_of_time_flags() {
        let mut clock = Clock::new(
            TimeControl::Canadian {
                main: secs(1),
                stones: 5,
                period: secs(3),
            },
            Some(Color::Black),
        );
        clock.tick(secs(1));
        assert_eq!(clock.tick(secs(3)), TickOutcome::Flagged(Color::Black));
    }

    #[test]
    fn move_switches_the_active_player() {
        let mut clock = Clock::new(
            TimeControl::Absolute { main: secs(30) },
            Some(Color::Black),
        );
        clock.on_move(Color::Black);
        assert_eq!(clock.active(), Some(Color::White));
        clock.on_move(Color::White);
        assert_eq!(clock.active(), Some(Color::Black));
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(secs(0)), "00:00");
        assert_eq!(format_duration(secs(65)), "01:05");
        assert_eq!(format_duration(secs(3725)), "1:02:05");
    }

    #[test]
    fn server_main_time_update_is_clamped() {
        let mut clock = Clock::new(
            TimeControl::Absolute { main: secs(30) },
            Some(Color::Black),
        );
        clock.set_main(Color::White, 12.5);
        assert_eq!(clock.player(Color::White).main, Duration::from_millis(12_500));
        clock.set_main(Color::White, -3.0);
        assert_eq!(clock.player(Color::White).main, Duration::ZERO);
    }

    #[test]
    fn sgf_time_control_roundtrips_all_variants() {
        let variants = [
            TimeControl::None,
            TimeControl::Absolute { main: secs(600) },
            TimeControl::Byoyomi {
                main: secs(600),
                periods: 5,
                period: secs(30),
            },
            TimeControl::Fischer {
                main: secs(300),
                increment: secs(10),
            },
            TimeControl::Canadian {
                main: secs(900),
                stones: 25,
                period: secs(300),
            },
        ];
        for tc in variants {
            let (tm, ot) = tc.to_sgf_fields();
            let parsed = TimeControl::from_sgf_fields(tm, ot.as_deref());
            assert_eq!(parsed, tc);
        }
    }
}
