//! Offline memory consolidation (the "dream" pipeline) plus its FFI payload types.
//!
//! The kernel side is pure computation: `idle_pipeline` drives
//! `trace_analyzer` → `synthesis` → `curator` over session transcripts fed in
//! by the SDK. Storage, embeddings, and retrieval I/O live in the SDKs.
//! Working-context memory management (paging, pressure) is `crate::mm`.

pub mod curator;
pub mod durable;
pub mod idle_pipeline;
pub mod semantic;
pub mod synthesis;
pub mod trace_analyzer;
