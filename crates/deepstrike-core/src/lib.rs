//! # DeepStrike Core
//!
//! Cross-language agent runtime kernel — pure computation, zero I/O.
//!
//! This crate provides the core state machines, data structures, and algorithms
//! for the DeepStrike agent framework. It is designed to be embedded via FFI
//! bindings (PyO3, napi-rs, wasm-bindgen) into any language runtime.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
//! │ Python SDK  │  │ Node.js SDK │  │  WASM SDK   │
//! └──────┬──────┘  └──────┬──────┘  └──────┬──────┘
//!        └────────┬───────┴────────┬───────┘
//!                 │  deepstrike-core │
//!                 └─────────────────┘
//! ```
//!
//! ## Design Principles
//!
//! - **Pure computation**: No I/O, no async, no network calls
//! - **State machine driven**: SDK feeds events, kernel returns actions
//! - **Zero-copy where possible**: CompactString, borrowed slices
//! - **Compile-time safety**: Ownership, Send+Sync, exhaustive matches

pub mod context;
pub mod governance;
pub mod harness;
pub mod memory;
pub mod mm;
pub mod orchestration;
pub mod proc;
pub mod runtime;
pub mod scheduler;
pub mod signals;
pub mod syscall;
pub mod types;

// Re-export key types at crate root for convenience
pub use governance::quota::ResourceQuota;
pub use mm::{
    plan_eviction, EvictionOp, EvictionPlan, Handle, HandleId, HandleKind, HandleTable,
    MemoryTierHint, PageInEntry, Residency,
};
pub use proc::{AgentProcess, ProcessState};
pub use scheduler::tcb::{BudgetLedger, TaskId, TaskLifecycle, TaskTable, Tcb, WaitReason};
pub use syscall::{Disposition, Syscall};
pub use runtime::session::SessionEvent;
pub use runtime::{
    category_for_kind, primitive_for_kind, reconstruct_messages_with_fallback,
    rebuild_os_snapshot_from_events, KernelEventCategory,
    Primitive, KERNEL_ABI_VERSION, KernelAction, KernelInput, OsSnapshot,
    KernelInputEvent, KernelObservation, KernelPressureAction, KernelRuntime, KernelStep,
};
pub use types::agent::{
    AgentCapabilityFilter, AgentIdentity, AgentIsolation, AgentRole, AgentRunSpec,
    ContextInheritance, IsolationManifest, LoopRoundSpec,
};
pub use types::capability::{
    CapabilityCommand, CapabilityDescriptor, CapabilityKind, CapabilityLease, CapabilityManifest,
};
pub use types::contract::{AcceptanceCriterion, VerificationContract};
pub use types::error::{DeepStrikeError, Result};
pub use types::message::{Message, ToolCall, ToolResult};
pub use types::milestone::{
    MilestoneCheckResult, MilestoneContract, MilestonePhase, MilestoneRollbackPolicy,
    MilestoneUnlockPolicy, MilestoneVerifier, RetryPolicy,
};
pub use types::signal::RuntimeSignal;
pub use types::task::{RuntimeTask, TaskLane};
