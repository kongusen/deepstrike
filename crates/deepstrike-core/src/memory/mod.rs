//! Durable memory contracts plus pure record-level curation helpers.
//!
//! Storage and extraction I/O live in SDK hosts. Every durable mutation enters the kernel through
//! `WriteMemory`; there is deliberately no second idle-consolidation state machine.

pub mod curator;
pub mod durable;
