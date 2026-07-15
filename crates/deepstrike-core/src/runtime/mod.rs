//! Runtime event types shared across SDK bindings.
//! I/O (append/read) lives in each language SDK — the kernel stays pure.

pub mod event_log;
pub mod kernel;
pub mod replay;
pub mod repair;
pub mod session;

pub use kernel::{
    KERNEL_ABI_VERSION, KernelAction, KernelEffect, KernelFault, KernelFaultCode, KernelInput,
    KernelInputEvent, KernelLifecycle, KernelObservation, KernelPressureAction, KernelRuntime,
    KernelStep,
};

pub use repair::{
    pending_tool_calls_from_messages, reconstruct_messages_with_fallback, repair_events,
    repair_events_with_cap, repair_llm_completed, repair_llm_completed_with_cap,
    sanitize_recovery_text, sanitize_recovery_text_bounded,
};
pub use event_log::{
    category_for_kind, primitive_for_kind, KernelEventCategory, Primitive,
};
pub use replay::{
    rebuild_os_snapshot_from_events, BudgetExceededRecord,
    OsSnapshot, ProcessRecord, SignalDeliveryDisposedRecord, SuspendRecord,
};
pub use session::{ProviderReplay, SessionEvent};
