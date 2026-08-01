//! Typed Rust surface for the Canonical Kernel ABI.
//!
//! This module deliberately re-exports the core-owned wire values instead of wrapping them in
//! SDK JSON DTOs. Rust callers therefore prepare a typed [`WireEnvelope`], receive the closed
//! [`CanonicalPreparation`] enum, persist [`KernelRecord::record_bytes`], and commit with the
//! digest core produced. There is no production direct-step method.

pub use deepstrike_core::runtime::kernel::wire::{
    CanonicalKernel, CheckpointAdvice, CheckpointBoundary, ConfigDefaults, Digest, DurableHead,
    EffectKind, EffectsDisposition, InputId, KernelEffect, KernelFault, KernelFaultCode,
    KernelInput, KernelPreparation, KernelRecord, KernelTerminal, OperationId, OperationLifecycle,
    PlannedStep, PrepareToken, RestoreCost, StepDisposition, TailUsage, TerminalDisposition,
    WireEnvelope, WireU64, canonical_digest,
};

pub type CanonicalPreparation = KernelPreparation<KernelRecord, PlannedStep>;
pub type CanonicalCommit = deepstrike_core::runtime::kernel::wire::CommittedTransition<PlannedStep>;
pub type CanonicalCheckpoint = deepstrike_core::runtime::kernel::wire::KernelCheckpoint;
pub type CanonicalCheckpointCandidate = deepstrike_core::runtime::kernel::wire::CheckpointCandidate;

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{CanonicalKernel, CanonicalPreparation, WireEnvelope};

    #[test]
    fn typed_rust_api_reads_the_shared_canonical_golden() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/kernel-wire/golden_lifecycle_agent_root.json"
        ))
        .expect("canonical fixture");
        let envelope: WireEnvelope =
            serde_json::from_value(fixture["links"][0]["envelope"].clone())
                .expect("typed envelope");
        let mut kernel = CanonicalKernel::default();

        let CanonicalPreparation::Prepared(prepared) = kernel.prepare(&envelope) else {
            panic!("golden configure must prepare");
        };
        assert_eq!(
            prepared.record.record_digest().as_str(),
            fixture["genesis_digest"].as_str().unwrap()
        );
        assert_eq!(
            std::str::from_utf8(prepared.record.record_bytes().as_slice()).unwrap(),
            serde_json::to_string(&fixture["links"][0]["record"]).unwrap()
        );
    }
}
