//! Operation terminals (spec §7.12).
//!
//! A terminal is the opposite of an effect: it requires no resolution, is allocated no effect id,
//! and an operation commits **at most one** for its whole life. Both facts are expressed in the
//! types rather than left to convention:
//!
//! * [`StepDisposition`] makes "a committed step publishes effects **or** a terminal" a union
//!   instead of a `(Vec<KernelEffect>, Option<KernelTerminal>)` pair that can hold both;
//! * [`TerminalSlot`] makes "exactly once" an API a second commit cannot pass, rather than an
//!   invariant each of four hosts re-implements (recovery-chain distortion R-C2 #6: terminal
//!   deduplication currently relies on host convention).
//!
//! The usage report is committed **inside** the terminal and nowhere else, so there is no second
//! place a run's accounting can disagree with itself.

use serde::{Deserialize, Serialize};

use super::command::CancellationReason;
use super::effect::{KernelEffect, ProviderMessage};
use super::scalar::{NodeId, WireU64, WorkflowId};

// ---------------------------------------------------------------------------------------------
// §7.12 · the terminal union
// ---------------------------------------------------------------------------------------------

/// How an operation ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelTerminal {
    /// An agent root finished its loop.
    Agent(AgentTerminal),
    /// A workflow root finished its DAG. A workflow completion commits this terminal **itself** —
    /// there is no external "complete the run" input, which is the only reason the historical
    /// `CompleteRun` existed.
    Workflow(WorkflowTerminal),
    /// The operation was cancelled. Cancellation is never an effect failure: it arrives only
    /// through `HostControl::Cancel`.
    Cancelled(CancelledTerminal),
    /// The kernel itself could not continue — a recovery ladder ran out, an invariant broke.
    Failed(FailedTerminal),
}

impl KernelTerminal {
    /// The one place a run's accounting is committed.
    pub fn usage(&self) -> &UsageReport {
        match self {
            Self::Agent(terminal) => &terminal.usage,
            Self::Workflow(terminal) => &terminal.usage,
            Self::Cancelled(terminal) => &terminal.usage,
            Self::Failed(terminal) => &terminal.usage,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTerminal {
    pub result: LoopResult,
    pub usage: UsageReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTerminal {
    pub outcome: WorkflowOutcome,
    pub usage: UsageReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelledTerminal {
    pub reason: CancellationReason,
    pub usage: UsageReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailedTerminal {
    pub failure: KernelFailure,
    pub usage: UsageReport,
}

/// What an agent loop produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopResult {
    pub termination: TerminationReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<ProviderMessage>,
    pub turns_used: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace_decision: Option<PaceDecision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaceDecision {
    pub action: PaceAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<WireU64>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coerced_from: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceAction {
    Continue,
    Sleep,
    Stop,
}

/// Why the loop stopped.
///
/// `user_abort` and a generic `error` are deliberately absent: a cancellation is a
/// [`KernelTerminal::Cancelled`] and a kernel failure is a [`KernelTerminal::Failed`]. Folding
/// them back in here would give the same event two representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    Completed,
    MaxTurns,
    TokenBudget,
    Deadline,
    /// The reactive recovery ladder for a provider context overflow ran out.
    ContextOverflow,
    /// Repeat fuse escalation — the agent kept re-issuing the same call. Distinct from `MaxTurns`,
    /// which a productive run can also reach.
    NoProgress,
    MilestoneExceeded,
}

/// Resource accounting for one operation. No wall clock and no duration: elapsed time is a host
/// observation, derivable from the envelope times already in the journal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReport {
    pub input_tokens: WireU64,
    pub output_tokens: WireU64,
    pub turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<WireU64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowOutcome {
    pub workflow_id: WorkflowId,
    pub status: WorkflowStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_nodes: Vec<NodeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_nodes: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Completed,
    Failed,
    Cancelled,
}

/// A kernel-side failure, as opposed to a host effect failure. This is what a
/// [`HostEffectFailure`](super::effect::HostEffectFailure) escalates *into* after the kernel has
/// made its one policy decision (DEC-5) and decided it cannot continue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelFailure {
    pub code: KernelFailureCode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelFailureCode {
    ProviderRecoveryExhausted,
    OutputRecoveryExhausted,
    /// A host effect failed and no recovery ladder applied.
    HostEffectFailed,
    ResourceExhausted,
    InvariantViolated,
}

// ---------------------------------------------------------------------------------------------
// exactly-once
// ---------------------------------------------------------------------------------------------

/// What a single committed step publishes.
///
/// A union, not a record with an optional terminal beside an effect list: "terminal and terminal
/// observations land in the same committed step, and that step publishes no effect" is then a
/// property of the type, not of every caller remembering to clear the other field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepDisposition {
    Effects(EffectsDisposition),
    Terminal(TerminalDisposition),
}

impl StepDisposition {
    pub fn effects(&self) -> &[KernelEffect] {
        match self {
            Self::Effects(disposition) => &disposition.effects,
            Self::Terminal(_) => &[],
        }
    }

    pub fn terminal(&self) -> Option<&KernelTerminal> {
        match self {
            Self::Effects(_) => None,
            Self::Terminal(disposition) => Some(&disposition.terminal),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal(_))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectsDisposition {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<KernelEffect>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDisposition {
    pub terminal: KernelTerminal,
}

/// The operation's single terminal slot.
///
/// [`TerminalSlot::commit`] succeeds once and refuses every later attempt, including an identical
/// one. That is the exactly-once invariant of §7.12 as an API: no caller can double-commit by
/// forgetting to check first, and a resumed operation that re-derives its terminal is told so
/// instead of emitting a second one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerminalSlot {
    terminal: Option<KernelTerminal>,
}

impl TerminalSlot {
    pub fn empty() -> Self {
        Self { terminal: None }
    }

    pub fn commit(
        &mut self,
        terminal: KernelTerminal,
    ) -> Result<&KernelTerminal, Box<TerminalAlreadyCommitted>> {
        if let Some(committed) = &self.terminal {
            return Err(Box::new(TerminalAlreadyCommitted {
                committed: committed.clone(),
                rejected: terminal,
            }));
        }
        Ok(self.terminal.insert(terminal))
    }

    pub fn get(&self) -> Option<&KernelTerminal> {
        self.terminal.as_ref()
    }

    pub fn is_committed(&self) -> bool {
        self.terminal.is_some()
    }
}

/// A second terminal was offered for an operation that already has one.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalAlreadyCommitted {
    pub committed: KernelTerminal,
    pub rejected: KernelTerminal,
}

impl std::fmt::Display for TerminalAlreadyCommitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this operation has already committed a terminal")
    }
}

impl std::error::Error for TerminalAlreadyCommitted {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::*;

    fn fixture(name: &str) -> Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/kernel-wire")
            .join(name);
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"))
    }

    fn keys(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    out.insert(key.clone());
                    keys(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
            _ => {}
        }
    }

    fn usage() -> UsageReport {
        UsageReport {
            input_tokens: WireU64::new(18_402),
            output_tokens: WireU64::new(2_117),
            turns: 7,
            cached_input_tokens: None,
        }
    }

    fn samples() -> Vec<KernelTerminal> {
        vec![
            KernelTerminal::Agent(AgentTerminal {
                result: LoopResult {
                    termination: TerminationReason::Completed,
                    final_message: None,
                    turns_used: 7,
                    pace_decision: None,
                },
                usage: usage(),
            }),
            KernelTerminal::Workflow(WorkflowTerminal {
                outcome: WorkflowOutcome {
                    workflow_id: WorkflowId::new("wf-1").unwrap(),
                    status: WorkflowStatus::Completed,
                    completed_nodes: vec![NodeId::new("node-a").unwrap()],
                    failed_nodes: Vec::new(),
                },
                usage: usage(),
            }),
            KernelTerminal::Cancelled(CancelledTerminal {
                reason: CancellationReason::User,
                usage: usage(),
            }),
            KernelTerminal::Failed(FailedTerminal {
                failure: KernelFailure {
                    code: KernelFailureCode::ProviderRecoveryExhausted,
                    message: "context overflow ladder exhausted".to_string(),
                },
                usage: usage(),
            }),
        ]
    }

    // -----------------------------------------------------------------------------------------
    // shape
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_terminal_has_four_shapes_and_every_one_commits_usage_exactly_once() {
        let mut tags = BTreeSet::new();
        for terminal in samples() {
            let value = serde_json::to_value(&terminal).unwrap();
            tags.insert(value["kind"].as_str().unwrap().to_string());
            assert!(
                value.get("usage").is_some(),
                "every terminal commits the usage report: {value}"
            );
            // and only once — no second usage-shaped field anywhere below it
            let mut all = BTreeSet::new();
            keys(&value, &mut all);
            assert!(
                !all.contains("usage_report") && !all.contains("budget_usage"),
                "usage travels in exactly one field: {value}"
            );
            let back: KernelTerminal = serde_json::from_value(value).unwrap();
            assert_eq!(back, terminal);
        }
        assert_eq!(
            tags,
            BTreeSet::from([
                "agent".to_string(),
                "cancelled".to_string(),
                "failed".to_string(),
                "workflow".to_string(),
            ])
        );
    }

    #[test]
    fn a_terminal_requires_no_resolution_and_carries_no_effect_id() {
        for terminal in samples() {
            let mut all = BTreeSet::new();
            keys(&serde_json::to_value(&terminal).unwrap(), &mut all);
            for banned in ["effect_id", "causation_input_id", "resolution"] {
                assert!(
                    !all.contains(banned),
                    "a terminal is not an effect; it must not carry {banned:?}"
                );
            }
        }
    }

    #[test]
    fn a_terminal_carries_no_host_wall_clock() {
        for terminal in samples() {
            let mut all = BTreeSet::new();
            keys(&serde_json::to_value(&terminal).unwrap(), &mut all);
            for banned in [
                "now_ms",
                "observed_at_ms",
                "timestamp",
                "timestamp_ms",
                "started_at_ms",
                "completed_at_ms",
                "wall_clock_ms",
                "duration_ms",
            ] {
                assert!(!all.contains(banned), "terminal must not carry {banned:?}");
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // exactly once (§7.12 invariant 1)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_terminal_slot_accepts_exactly_one_terminal() {
        let mut slot = TerminalSlot::empty();
        assert!(!slot.is_committed());
        assert!(slot.get().is_none());

        let first = samples().into_iter().next().unwrap();
        slot.commit(first.clone()).expect("first terminal commits");
        assert!(slot.is_committed());
        assert_eq!(slot.get(), Some(&first));

        // a second terminal — of any shape, including an identical one — is refused
        for second in samples() {
            let rejected = slot
                .commit(second)
                .expect_err("an operation has at most one terminal");
            assert_eq!(rejected.committed, first);
        }
        assert_eq!(slot.get(), Some(&first));
    }

    #[test]
    fn a_committed_step_publishes_effects_or_a_terminal_but_never_both() {
        let effects = StepDisposition::Effects(EffectsDisposition {
            effects: vec![KernelEffect {
                effect_id: EffectId::new("op-1:step:1:effect:0").unwrap(),
                causation_input_id: InputId::new("in-1").unwrap(),
                effect: EffectKind::EvaluateMilestone(EvaluateMilestoneEffect {
                    request: MilestoneRequest {
                        contract_id: "brief-quality-primary".to_string(),
                        phase_id: "phase-1".to_string(),
                    },
                }),
            }],
        });
        assert!(effects.terminal().is_none());
        assert_eq!(effects.effects().len(), 1);

        let terminal = StepDisposition::Terminal(TerminalDisposition {
            terminal: samples().into_iter().next().unwrap(),
        });
        assert!(terminal.terminal().is_some());
        assert!(
            terminal.effects().is_empty(),
            "the terminal step publishes no effect"
        );

        // the wire shape cannot express the mixture either
        let mixed = json!({
            "kind": "terminal",
            "terminal": serde_json::to_value(samples().into_iter().next().unwrap()).unwrap(),
            "effects": [],
        });
        assert!(
            serde_json::from_value::<StepDisposition>(mixed).is_err(),
            "a step must not carry both a terminal and an effect list"
        );
    }

    // -----------------------------------------------------------------------------------------
    // strictness
    // -----------------------------------------------------------------------------------------

    #[test]
    fn unknown_terminal_kinds_and_fields_are_rejected() {
        let unknown_kind = json!({ "kind": "done", "usage": { "input_tokens": "1", "output_tokens": "1", "turns": 1 } });
        assert!(serde_json::from_value::<KernelTerminal>(unknown_kind).is_err());

        let unknown_field = json!({
            "kind": "cancelled",
            "reason": "user",
            "usage": { "input_tokens": "1", "output_tokens": "1", "turns": 1 },
            "now_ms": 1753747203000u64,
        });
        assert!(serde_json::from_value::<KernelTerminal>(unknown_field).is_err());

        let numeric_tokens = json!({
            "kind": "cancelled",
            "reason": "user",
            "usage": { "input_tokens": 1, "output_tokens": "1", "turns": 1 },
        });
        assert!(serde_json::from_value::<KernelTerminal>(numeric_tokens).is_err());
    }

    // -----------------------------------------------------------------------------------------
    // goldens
    // -----------------------------------------------------------------------------------------

    #[test]
    fn terminal_goldens_round_trip_unchanged() {
        let mut covered = BTreeSet::new();
        for name in [
            "golden_terminal_agent.json",
            "golden_terminal_workflow.json",
            "golden_terminal_cancelled.json",
            "golden_terminal_failed.json",
        ] {
            let golden = fixture(name);
            let terminal: KernelTerminal = serde_json::from_value(golden.clone())
                .unwrap_or_else(|e| panic!("{name} does not decode: {e}"));
            assert_eq!(
                serde_json::to_value(&terminal).unwrap(),
                golden,
                "{name}: round-trip changed the document"
            );
            covered.insert(golden["kind"].as_str().unwrap().to_string());
        }
        assert_eq!(covered.len(), 4, "one golden per terminal shape");
    }
}
