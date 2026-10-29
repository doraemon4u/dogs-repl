//! The New Game flow, shared by the TUI overlay and the REPL prompts.
//!
//! Steps: board size → mode → rule set → (vs AI only) bot. Steps already
//! answered by an inline `new …` argument are skipped. This module holds the
//! choices only; each front end draws them its own way.

use crate::engine::rules::SUPPORTED_SIZES;
use crate::session::store::BotInfo;
use crate::session::Command;
use crate::transport::rest_client::{ChallengeClock, ChallengeOptions};
use crate::ui::commands::{GameMode, NewGameSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Size,
    Mode,
    Rules,
    Bot,
}

impl Step {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Size => "1/3 棋盘 Board size",
            Self::Mode => "2/3 模式 Mode",
            Self::Rules => "3/3 规则 Rules",
            Self::Bot => "AI 对手 Choose an OGS bot",
        }
    }
}

/// One selectable line. Disabled lines show why they cannot be picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub disabled: Option<String>,
}

/// Result of confirming the highlighted choice.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Moved to the next step.
    Next,
    /// Every step is answered.
    Done(NewGameSpec),
    /// The highlighted choice cannot be used; the text says why.
    Blocked(String),
}

#[derive(Debug, Clone)]
pub struct NewGameWizard {
    pub spec: NewGameSpec,
    pub selected: usize,
    logged_in: bool,
    bots: Vec<BotInfo>,
    /// Steps the user answered in this flow, for Esc/back.
    answered: Vec<Step>,
}

impl NewGameWizard {
    pub fn new(spec: NewGameSpec, logged_in: bool, bots: Vec<BotInfo>) -> Self {
        let mut wizard = Self {
            spec,
            selected: 0,
            logged_in,
            bots,
            answered: Vec::new(),
        };
        wizard.reset_selection();
        wizard
    }

    /// The step waiting for an answer, or `None` when finished.
    pub fn step(&self) -> Option<Step> {
        if self.spec.size.is_none() {
            Some(Step::Size)
        } else if self.spec.mode.is_none() {
            Some(Step::Mode)
        } else if self.spec.rules.is_none() {
            Some(Step::Rules)
        } else if self.spec.mode == Some(GameMode::VsAi) && self.spec.bot.is_none() {
            Some(Step::Bot)
        } else {
            None
        }
    }

    pub fn choices(&self) -> Vec<Choice> {
        let enabled = |label: String| Choice { label, disabled: None };
        match self.step() {
            Some(Step::Size) => SUPPORTED_SIZES
                .iter()
                .map(|size| enabled(format!("{size}×{size}")))
                .collect(),
            Some(Step::Mode) => GameMode::ALL
                .iter()
                .map(|mode| Choice {
                    label: mode.label().to_string(),
                    disabled: (*mode == GameMode::VsAi && !self.logged_in)
                        .then(|| "login required".to_string()),
                })
                .collect(),
            Some(Step::Rules) => self
                .mode()
                .rule_sets()
                .iter()
                .map(|rules| {
                    enabled(format!(
                        "{} · komi {}",
                        rules.label(),
                        rules.default_komi()
                    ))
                })
                .collect(),
            Some(Step::Bot) => {
                if self.bots.is_empty() {
                    vec![Choice {
                        label: "(no bots reported online yet)".into(),
                        disabled: Some(
                            "wait for the bot list, or type `new ai <size> <rules> <bot>`".into(),
                        ),
                    }]
                } else {
                    self.bots
                        .iter()
                        .map(|bot| match &bot.ranking {
                            Some(rank) => enabled(format!("{} [{rank}]", bot.username)),
                            None => enabled(bot.username.clone()),
                        })
                        .collect()
                }
            }
            None => Vec::new(),
        }
    }

    fn mode(&self) -> GameMode {
        self.spec.mode.unwrap_or(GameMode::Local)
    }

    pub fn move_by(&mut self, delta: i32) {
        let len = self.choices().len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected as i32 + delta).rem_euclid(len as i32) as usize;
    }

    /// Picks choice `index` (used by number keys and the REPL).
    pub fn choose(&mut self, index: usize) -> Outcome {
        let choices = self.choices();
        let Some(choice) = choices.get(index) else {
            return Outcome::Blocked(format!("choose 1–{}", choices.len()));
        };
        if let Some(reason) = &choice.disabled {
            return Outcome::Blocked(reason.clone());
        }
        let Some(step) = self.step() else {
            return Outcome::Done(self.spec.clone());
        };
        match step {
            Step::Size => self.spec.size = Some(SUPPORTED_SIZES[index]),
            Step::Mode => {
                let mode = GameMode::ALL[index];
                self.spec.mode = Some(mode);
                // A rule set typed inline may not exist on OGS.
                if self.spec.rules.is_some_and(|rules| !mode.rule_sets().contains(&rules)) {
                    self.spec.rules = None;
                }
            }
            Step::Rules => self.spec.rules = Some(self.mode().rule_sets()[index]),
            Step::Bot => self.spec.bot = Some(self.bots[index].username.clone()),
        }
        self.answered.push(step);
        self.reset_selection();
        if self.spec.is_complete() {
            Outcome::Done(self.spec.clone())
        } else {
            Outcome::Next
        }
    }

    pub fn confirm(&mut self) -> Outcome {
        self.choose(self.selected)
    }

    /// Undoes the last answer. Returns `false` when there is nothing to undo,
    /// so the caller can close the flow.
    pub fn back(&mut self) -> bool {
        let Some(step) = self.answered.pop() else {
            return false;
        };
        match step {
            Step::Size => self.spec.size = None,
            Step::Mode => self.spec.mode = None,
            Step::Rules => self.spec.rules = None,
            Step::Bot => self.spec.bot = None,
        }
        self.reset_selection();
        true
    }

    /// Highlights the first enabled choice.
    fn reset_selection(&mut self) {
        self.selected = self
            .choices()
            .iter()
            .position(|choice| choice.disabled.is_none())
            .unwrap_or(0);
    }

    /// Summary of the answers so far, e.g. `19×19 · 摆谱 · Chinese`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(size) = self.spec.size {
            parts.push(format!("{size}×{size}"));
        }
        if let Some(mode) = self.spec.mode {
            parts.push(mode.label().to_string());
        }
        if let Some(rules) = self.spec.rules {
            parts.push(rules.label().to_string());
        }
        parts.join(" · ")
    }
}

/// Turns a complete New Game choice into the session command that starts it.
pub fn start_command(spec: &NewGameSpec, bots: &[BotInfo]) -> Result<Command, String> {
    let size = spec.size.ok_or("board size not chosen")?;
    let rules = spec.rules.ok_or("rule set not chosen")?;
    match spec.mode.ok_or("mode not chosen")? {
        GameMode::Local => Ok(Command::NewLocalGame {
            size,
            rules,
            moves: Vec::new(),
        }),
        GameMode::VsAi => {
            let bot = spec.bot.clone().ok_or("bot not chosen")?;
            let ogs_rules = rules
                .ogs_name()
                .ok_or_else(|| format!("{} cannot be played on OGS", rules.label()))?;
            let opponent_id = bots
                .iter()
                .find(|known| known.username.eq_ignore_ascii_case(&bot))
                .map(|known| known.id);
            Ok(Command::CreateChallenge(ChallengeOptions {
                opponent: bot,
                opponent_id,
                board_size: size,
                rules: ogs_rules.to_string(),
                ranked: false,
                handicap: 0,
                komi: rules.default_komi(),
                challenger_color: "automatic".to_string(),
                clock: ChallengeClock::default(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::rules::RuleSet;

    fn bots() -> Vec<BotInfo> {
        vec![BotInfo {
            id: 7,
            username: "katago".into(),
            ranking: Some("9d".into()),
        }]
    }

    #[test]
    fn local_flow_walks_size_mode_rules() {
        let mut wizard = NewGameWizard::new(NewGameSpec::default(), false, Vec::new());
        assert_eq!(wizard.step(), Some(Step::Size));
        assert_eq!(wizard.choose(1), Outcome::Next); // 13×13
        assert_eq!(wizard.step(), Some(Step::Mode));
        // vs AI is disabled when logged out, so the cursor starts on 摆谱.
        assert_eq!(wizard.selected, 1);
        assert!(matches!(wizard.choose(0), Outcome::Blocked(reason) if reason.contains("login")));
        assert_eq!(wizard.confirm(), Outcome::Next);
        assert_eq!(wizard.choices().len(), 3, "ancient rules offered for 摆谱");
        let Outcome::Done(spec) = wizard.choose(2) else { panic!() };
        assert_eq!(spec.size, Some(13));
        assert_eq!(spec.rules, Some(RuleSet::AncientChinese));
        let Command::NewLocalGame { size, rules, .. } = start_command(&spec, &[]).unwrap() else {
            panic!()
        };
        assert_eq!((size, rules), (13, RuleSet::AncientChinese));
    }

    #[test]
    fn ai_flow_offers_only_ogs_rules_and_builds_an_unranked_challenge() {
        let spec = NewGameSpec {
            size: Some(19),
            ..NewGameSpec::default()
        };
        let mut wizard = NewGameWizard::new(spec, true, bots());
        assert_eq!(wizard.step(), Some(Step::Mode));
        assert_eq!(wizard.choose(0), Outcome::Next);
        assert_eq!(wizard.choices().len(), 2, "no ancient rules on OGS");
        assert_eq!(wizard.choose(1), Outcome::Next);
        assert_eq!(wizard.step(), Some(Step::Bot));
        let Outcome::Done(spec) = wizard.confirm() else { panic!() };
        let Command::CreateChallenge(options) = start_command(&spec, &bots()).unwrap() else {
            panic!()
        };
        assert_eq!(options.opponent, "katago");
        assert_eq!(options.opponent_id, Some(7));
        assert_eq!(options.board_size, 19);
        assert_eq!(options.rules, "japanese");
        assert!(!options.ranked);
        assert_eq!(options.handicap, 0);
        assert_eq!(options.challenger_color, "automatic");
    }

    #[test]
    fn back_steps_through_answers_then_closes() {
        let mut wizard = NewGameWizard::new(NewGameSpec::default(), true, bots());
        wizard.choose(0);
        assert_eq!(wizard.step(), Some(Step::Mode));
        assert!(wizard.back());
        assert_eq!(wizard.step(), Some(Step::Size));
        assert!(!wizard.back());
    }

    #[test]
    fn empty_bot_list_blocks_with_a_hint() {
        let spec = NewGameSpec {
            size: Some(9),
            mode: Some(GameMode::VsAi),
            rules: Some(RuleSet::Chinese),
            bot: None,
        };
        let mut wizard = NewGameWizard::new(spec, true, Vec::new());
        assert!(matches!(wizard.confirm(), Outcome::Blocked(reason) if reason.contains("new ai")));
    }
}
