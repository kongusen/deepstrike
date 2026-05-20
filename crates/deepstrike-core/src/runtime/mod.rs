//! Runtime event types shared across SDK bindings.
//! I/O (append/read) lives in each language SDK — the kernel stays pure.

pub mod session;

pub use session::{ProviderReplay, SessionEvent};
