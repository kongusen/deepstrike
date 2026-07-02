//! O6: RepeatFuse — the hard rungs of the no-progress escalation ladder.
//!
//! The 2c salience-footer STOP (context/renderer.rs) is the SOFT rung: at ≥2 consecutive turns of
//! the identical tool call (same name AND args) it injects a `STOP:` line at the prompt's
//! peak-attention position and bets the model self-corrects. This module holds the config for the
//! two rungs above it, enforced in the state machine's gate (scheduler/state_machine/gate.rs):
//!
//! - **deny** (`deny_after`, default 5): the turn is rolled back like a governance deny and a
//!   directive note — WHY it was denied, WHAT to do instead — is fed back to the model.
//! - **terminate** (`terminate_after`, default 8): the run ends with
//!   [`TerminationReason::NoProgress`](crate::types::result::TerminationReason) after one final
//!   no-tools report turn, so embedders can tell "looped with no progress" from `MaxTurns`.
//!
//! The fuse keys on the SAME per-turn signature the 2c STOP uses (non-meta `name(args)` joined),
//! so a legit loop varying its args reads as distinct progress on every rung. A signature-based
//! fuse deliberately does NOT catch args-varying loops — the token/turn budgets remain the
//! backstop there (any mechanical detector would false-positive real iteration).

use serde::{Deserialize, Serialize};

/// Thresholds for the repeat fuse. `0` disables that rung individually; `enabled: false` disables
/// the whole fuse. Counting is per consecutive identical turn-signature: a different call (or the
/// same tool with different args) resets the streak to 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatFuseConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Consecutive identical turns before the call is DENIED (turn rolled back + directive note).
    #[serde(default = "default_deny_after")]
    pub deny_after: u32,
    /// Consecutive identical turns before the run TERMINATES with `NoProgress`.
    #[serde(default = "default_terminate_after")]
    pub terminate_after: u32,
}

fn default_enabled() -> bool { true }
fn default_deny_after() -> u32 { 5 }
fn default_terminate_after() -> u32 { 8 }

impl Default for RepeatFuseConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            deny_after: default_deny_after(),
            terminate_after: default_terminate_after(),
        }
    }
}
