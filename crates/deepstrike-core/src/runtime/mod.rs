//! Runtime event types shared across SDK bindings.
//! I/O (append/read) lives in each language SDK — the kernel stays pure.

pub mod event_log;
pub mod kernel;
pub mod replay;
pub mod repair;
pub mod session;

pub use kernel::{
    KERNEL_ABI_VERSION, KernelAction, KernelInput, KernelInputEvent, KernelObservation,
    KernelPressureAction, KernelRuntime, KernelStep,
};

pub use repair::{
    effective_provider_replay, pending_tool_calls_from_messages, reconstruct_messages_with_fallback,
    repair_events, repair_events_with_cap, repair_llm_completed, repair_llm_completed_with_cap,
    sanitize_recovery_text, sanitize_recovery_text_bounded, synthesize_provider_replay,
};
pub use event_log::{category_for_kind, KernelEventCategory, KERNEL_OBSERVATION_KINDS};
pub use replay::{
    rebuild_os_snapshot_from_events, session_log_has_required_categories, BudgetExceededRecord,
    OsSnapshot, ProcessRecord, SignalDisposedRecord, SuspendRecord,
};
pub use session::{ProviderReplay, SessionEvent};
