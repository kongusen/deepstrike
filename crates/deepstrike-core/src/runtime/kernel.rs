//! Canonical host/kernel ABI and scheduler observation projection.

mod observation;
pub mod wire;

pub use observation::{KernelObservation, KernelPressureAction, WorkflowSpawnFailure};
