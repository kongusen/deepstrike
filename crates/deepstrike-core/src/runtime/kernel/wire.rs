//! Canonical Kernel ABI — wire contract (revision 3).
//!
//! One envelope, five input classes, one root entry, strict tagged unions and cross-language
//! scalar rules. This is the only host/kernel input contract; no compatibility adapter or
//! inferred revision exists.
//!
//! Layout:
//!
//! | module | contract |
//! | --- | --- |
//! | [`scalar`] | §7.1.1 cross-language scalar and projection rules |
//! | [`envelope`] | §7.1 envelope, §7.2 five-class taxonomy, decode boundary |
//! | [`config`] | §7.3 configuration's position in the taxonomy (fields: Task 5) |
//! | [`root`] | §7.4 root entry, execution focus, logical start payloads |
//! | [`command`] | §7.5 host control plane |
//! | [`syscall`] | §7.6 P1 syscall requests and derived causation |
//! | [`event`] | §7.7 external events |
//! | [`effect`] | §7.9 effect outcome's position in the taxonomy (fields: Task 4) |
//! | [`checkpoint`] | §12.1 logical checkpoint + bounded tail |
//! | [`restore`] | §12.2 bounded-tail restore |

use serde::{Deserialize, Serialize};

pub mod binding;
pub mod checkpoint;
pub mod command;
pub mod config;
pub mod driver;
pub mod effect;
pub mod envelope;
pub mod event;
pub mod fault;
pub mod record;
pub mod restore;
pub mod root;
pub mod scalar;
pub mod syscall;
pub mod terminal;
pub mod transaction;

#[cfg(test)]
mod tests;

pub use binding::CanonicalKernel;
pub use checkpoint::*;
pub use command::*;
pub use config::*;
pub use driver::{CanonicalOperationDriver, PlannedStep};
pub use effect::*;
pub use envelope::*;
pub use event::*;
pub use fault::*;
pub use record::*;
pub use restore::*;
pub use root::*;
pub use scalar::*;
pub use syscall::*;
pub use terminal::*;
pub use transaction::*;

/// The single supported wire revision (§16.2). There is no negotiation, no adapter and no
/// inference from missing fields: anything else is a structured rejection.
///
/// Single source of truth — the bindings export this constant instead of each host hard-coding
/// its own copy.
pub const KERNEL_ABI_VERSION: u32 = 3;

/// Revision of the logical checkpoint format, carried on the wire as `checkpoint_version`.
///
/// This remains a separate revision axis so retired recovery-format values cannot collide with the
/// logical checkpoint boundary check.
pub const KERNEL_CHECKPOINT_VERSION: u32 = 2;

/// Absolute structural boundary applied **before** any JSON is parsed (§7.3).
///
/// These are build/construction-time safety limits, not operation configuration:
/// `OperationConfig.kernel_limits` may only tighten them, never widen them. Enforcing them
/// pre-parse is the point — a bound checked after `serde_json` has already materialised the
/// document is a bound the attacker already spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelBootstrapLimits {
    pub absolute_max_input_bytes: u32,
    pub absolute_max_json_depth: u16,
    pub absolute_max_collection_entries: u32,
}

impl KernelBootstrapLimits {
    /// 16 MiB matches the historical kernel input ceiling; depth and per-container entry bounds
    /// are new — the legacy decode path had neither.
    pub const DEFAULT: Self = Self {
        absolute_max_input_bytes: 16 * 1024 * 1024,
        absolute_max_json_depth: 64,
        absolute_max_collection_entries: 65_536,
    };
}

impl Default for KernelBootstrapLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}
