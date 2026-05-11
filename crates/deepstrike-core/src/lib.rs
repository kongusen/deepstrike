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
pub mod orchestration;
pub mod scheduler;
pub mod signals;
pub mod types;

// Re-export key types at crate root for convenience
pub use types::agent::AgentIdentity;
pub use types::error::{DeepStrikeError, Result};
pub use types::message::{Message, ToolCall, ToolResult};
pub use types::signal::RuntimeSignal;
pub use types::task::RuntimeTask;
