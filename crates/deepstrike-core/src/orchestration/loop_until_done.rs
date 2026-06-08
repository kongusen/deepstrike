//! Loop-until-done — drive an agent round after round until a stop predicate fires.
//!
//! For tasks with an unknown amount of work (keep investigating until no new findings;
//! keep fixing until no more errors). Mirrors [`super::gen_eval::GenEvalLoop`]: a pure
//! control state machine emitting **abstract actions**; the SDK spawns a worker per round
//! and feeds back what that round produced. No I/O, no clock.
//!
//! Termination is guaranteed *in-kernel*: a [`StopCondition::MaxRounds`] backstop is always
//! present (injected with [`DEFAULT_MAX_ROUNDS`] if the caller configured none), so the loop
//! cannot run forever regardless of SDK behavior. Wall-clock / token backstops are an
//! orthogonal concern owned by the SDK's [`crate::scheduler::policy::SchedulerBudget`]
//! wrapped around the loop — this state machine stays zero-clock.

/// Default hard round cap injected when the caller configures no [`StopCondition::MaxRounds`].
pub const DEFAULT_MAX_ROUNDS: u32 = 50;

/// A stop predicate. The loop stops as soon as **any** configured condition fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCondition {
    /// The round produced no new findings (`new_findings == 0`).
    NoNewFindings,
    /// The round reported no errors (`errors == 0`).
    NoErrors,
    /// Hard cap: stop once this many rounds have completed.
    MaxRounds(u32),
}

/// Loop configuration. Always carries a `MaxRounds` backstop after construction.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    conditions: Vec<StopCondition>,
}

impl LoopConfig {
    /// Build a config. If no [`StopCondition::MaxRounds`] is present, a
    /// [`DEFAULT_MAX_ROUNDS`] backstop is appended so termination is guaranteed.
    pub fn new(conditions: Vec<StopCondition>) -> Self {
        let has_max = conditions
            .iter()
            .any(|c| matches!(c, StopCondition::MaxRounds(_)));
        let mut conditions = conditions;
        if !has_max {
            conditions.push(StopCondition::MaxRounds(DEFAULT_MAX_ROUNDS));
        }
        Self { conditions }
    }

    pub fn conditions(&self) -> &[StopCondition] {
        &self.conditions
    }
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// What the SDK reports after running a round's worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundReport {
    pub new_findings: u32,
    pub errors: u32,
}

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    NoNewFindings,
    NoErrors,
    MaxRounds,
}

/// What the SDK should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopAction {
    /// Spawn the worker for round `round`, then call [`LoopUntilDone::feed`] with its report.
    Spawn { round: u32 },
    /// The loop converged.
    Done {
        rounds_used: u32,
        reason: StopReason,
    },
}

/// Loop-until-done control state machine.
#[derive(Debug)]
pub struct LoopUntilDone {
    config: LoopConfig,
    /// Round currently in flight (the last one emitted by `start`/`feed`).
    round: u32,
    done: bool,
}

impl LoopUntilDone {
    pub fn new(config: LoopConfig) -> Self {
        Self {
            config,
            round: 0,
            done: false,
        }
    }

    /// Begin the loop — always spawns round 1.
    pub fn start(&mut self) -> LoopAction {
        self.round = 1;
        LoopAction::Spawn { round: 1 }
    }

    /// Feed the just-completed round's report and get the next action.
    pub fn feed(&mut self, report: RoundReport) -> LoopAction {
        if self.done {
            return LoopAction::Done {
                rounds_used: self.round,
                reason: StopReason::MaxRounds,
            };
        }

        if let Some(reason) = self.first_triggered(&report) {
            self.done = true;
            return LoopAction::Done {
                rounds_used: self.round,
                reason,
            };
        }

        self.round += 1;
        LoopAction::Spawn { round: self.round }
    }

    /// Return the reason for the first stop condition (in configured order) that fires for
    /// the just-completed round (`self.round`).
    fn first_triggered(&self, report: &RoundReport) -> Option<StopReason> {
        for cond in &self.config.conditions {
            let hit = match cond {
                StopCondition::NoNewFindings => report.new_findings == 0,
                StopCondition::NoErrors => report.errors == 0,
                StopCondition::MaxRounds(max) => self.round >= *max,
            };
            if hit {
                return Some(match cond {
                    StopCondition::NoNewFindings => StopReason::NoNewFindings,
                    StopCondition::NoErrors => StopReason::NoErrors,
                    StopCondition::MaxRounds(_) => StopReason::MaxRounds,
                });
            }
        }
        None
    }

    pub fn is_done(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(new_findings: u32, errors: u32) -> RoundReport {
        RoundReport {
            new_findings,
            errors,
        }
    }

    #[test]
    fn default_injects_max_rounds_backstop() {
        let cfg = LoopConfig::default();
        assert!(
            cfg.conditions()
                .iter()
                .any(|c| matches!(c, StopCondition::MaxRounds(DEFAULT_MAX_ROUNDS)))
        );
    }

    #[test]
    fn explicit_max_rounds_not_duplicated() {
        let cfg = LoopConfig::new(vec![StopCondition::MaxRounds(5)]);
        let maxes: Vec<_> = cfg
            .conditions()
            .iter()
            .filter(|c| matches!(c, StopCondition::MaxRounds(_)))
            .collect();
        assert_eq!(maxes.len(), 1);
    }

    #[test]
    fn start_spawns_round_one() {
        let mut l = LoopUntilDone::new(LoopConfig::default());
        assert_eq!(l.start(), LoopAction::Spawn { round: 1 });
    }

    #[test]
    fn stops_on_no_new_findings() {
        let mut l = LoopUntilDone::new(LoopConfig::new(vec![StopCondition::NoNewFindings]));
        l.start();
        // round 1 found things → continue
        assert_eq!(l.feed(report(3, 0)), LoopAction::Spawn { round: 2 });
        // round 2 found nothing → stop
        assert_eq!(
            l.feed(report(0, 9)),
            LoopAction::Done {
                rounds_used: 2,
                reason: StopReason::NoNewFindings
            }
        );
        assert!(l.is_done());
    }

    #[test]
    fn stops_on_no_errors() {
        let mut l = LoopUntilDone::new(LoopConfig::new(vec![StopCondition::NoErrors]));
        l.start();
        assert_eq!(l.feed(report(0, 2)), LoopAction::Spawn { round: 2 });
        assert_eq!(
            l.feed(report(0, 0)),
            LoopAction::Done {
                rounds_used: 2,
                reason: StopReason::NoErrors
            }
        );
    }

    #[test]
    fn max_rounds_caps_the_loop() {
        let mut l = LoopUntilDone::new(LoopConfig::new(vec![StopCondition::MaxRounds(3)]));
        l.start();
        assert_eq!(l.feed(report(1, 1)), LoopAction::Spawn { round: 2 });
        assert_eq!(l.feed(report(1, 1)), LoopAction::Spawn { round: 3 });
        assert_eq!(
            l.feed(report(1, 1)),
            LoopAction::Done {
                rounds_used: 3,
                reason: StopReason::MaxRounds
            }
        );
    }

    #[test]
    fn default_backstop_terminates_unbounded_predicate() {
        // NoErrors configured but errors never reach 0 → default MaxRounds(50) must fire.
        let mut l = LoopUntilDone::new(LoopConfig::new(vec![StopCondition::NoErrors]));
        let mut action = l.start();
        let mut last = action;
        for _ in 0..100 {
            match action {
                LoopAction::Spawn { .. } => {
                    last = action;
                    action = l.feed(report(1, 1));
                }
                LoopAction::Done { .. } => break,
            }
        }
        let _ = last;
        assert_eq!(
            action,
            LoopAction::Done {
                rounds_used: DEFAULT_MAX_ROUNDS,
                reason: StopReason::MaxRounds
            }
        );
    }

    #[test]
    fn first_configured_condition_wins() {
        // Both NoNewFindings and NoErrors would fire on (0,0); configured order picks the first.
        let mut l = LoopUntilDone::new(LoopConfig::new(vec![
            StopCondition::NoNewFindings,
            StopCondition::NoErrors,
        ]));
        l.start();
        assert_eq!(
            l.feed(report(0, 0)),
            LoopAction::Done {
                rounds_used: 1,
                reason: StopReason::NoNewFindings
            }
        );
    }
}
