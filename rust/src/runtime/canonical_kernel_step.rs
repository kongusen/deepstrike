//! Durable Rust host for the canonical kernel ABI.
//!
//! Core prepares the typed envelope and record; the journal makes that record authoritative; only
//! then is the planned step committed and exposed to a runner. This module exposes no alternate
//! input vocabulary or direct-step escape hatch.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use crate::runtime::canonical_kernel::{
    CanonicalCommit, CanonicalKernel, CanonicalPreparation, InputId, KernelFault, KernelFaultCode,
    KernelInput, OperationId, PlannedStep, WireEnvelope, WireU64,
};
use crate::runtime::kernel_journal::{
    CheckpointCandidate, InstalledCheckpoint, JournalError, JournalRecordInput, KernelJournal,
};
use crate::{Error, Result};

// A moving journal may require several restore/retry passes, but a permanently contended host
// must fail closed instead of growing the async call stack indefinitely.
const MAX_TRANSITION_RECONCILIATIONS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum HostTransitionError {
    #[error("canonical kernel rejected input: {0}")]
    Rejected(KernelFault),
    #[error(
        "canonical record is durable but commit could not be published; runtime rebuilt from journal"
    )]
    RebuildRequired,
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Other(#[from] Error),
}

impl From<HostTransitionError> for Error {
    fn from(value: HostTransitionError) -> Self {
        match value {
            HostTransitionError::Rejected(_) | HostTransitionError::RebuildRequired => {
                Self::Other(value.to_string())
            }
            HostTransitionError::Journal(err) => Self::from(err),
            HostTransitionError::Other(err) => err,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CanonicalTransition {
    pub envelope: WireEnvelope,
    pub step_seq: u64,
    pub record_digest: String,
    pub planned_step: PlannedStep,
    pub checkpoint_advised: bool,
    pub replayed: bool,
}

/// The typed prepare → append → commit host boundary for one operation.
pub struct CanonicalKernelHost {
    kernel: Mutex<CanonicalKernel>,
    journal: Arc<dyn KernelJournal>,
    operation_id: String,
}

impl CanonicalKernelHost {
    pub fn new(
        kernel: CanonicalKernel,
        journal: Arc<dyn KernelJournal>,
        operation_id: impl Into<String>,
    ) -> Result<Self> {
        let operation_id = operation_id.into();
        if operation_id.is_empty() {
            return Err(Error::Other(
                "canonical kernel operation_id must not be empty".into(),
            ));
        }
        Ok(Self {
            kernel: Mutex::new(kernel),
            journal,
            operation_id,
        })
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn journal(&self) -> &Arc<dyn KernelJournal> {
        &self.journal
    }

    pub fn lifecycle(&self) -> crate::runtime::canonical_kernel::OperationLifecycle {
        self.kernel.lock().unwrap().lifecycle()
    }

    pub fn pending_effects(&self) -> Vec<crate::runtime::canonical_kernel::KernelEffect> {
        self.kernel
            .lock()
            .unwrap()
            .pending_effects()
            .cloned()
            .collect()
    }

    pub fn terminal(&self) -> Option<crate::runtime::canonical_kernel::KernelTerminal> {
        self.kernel.lock().unwrap().terminal().cloned()
    }

    pub fn attempt_id(&self, task_id: &str) -> Option<String> {
        self.kernel
            .lock()
            .unwrap()
            .attempt_id(task_id)
            .map(|attempt_id| attempt_id.as_str().to_string())
    }

    pub fn turn(&self) -> u32 {
        self.kernel.lock().unwrap().turn()
    }

    pub fn recovery_content_bytes(&self) -> Option<usize> {
        self.kernel.lock().unwrap().recovery_content_bytes()
    }

    pub fn preserved_refs(&self) -> Vec<String> {
        self.kernel.lock().unwrap().preserved_refs()
    }

    pub fn count_tokens(&self, text: &str) -> Option<u32> {
        self.kernel.lock().unwrap().count_tokens(text)
    }

    pub fn local_subagents_spawned(&self) -> usize {
        self.kernel.lock().unwrap().local_subagents_spawned() as usize
    }

    pub fn new_messages(&self) -> Vec<deepstrike_core::types::message::Message> {
        self.kernel.lock().unwrap().new_messages()
    }

    /// Stage the exact serialized typed envelope before attempting its durable append.
    pub async fn transition(&self, envelope: WireEnvelope) -> Result<CanonicalTransition> {
        if envelope.operation_id.as_str() != self.operation_id {
            return Err(Error::Other(
                "canonical envelope operation_id does not match host operation_id".into(),
            ));
        }
        let staged = serde_json::to_string(&envelope).map_err(|error| {
            Error::Other(format!("canonical envelope is not serializable: {error}"))
        })?;
        self.journal
            .stage_outbound_envelope(&self.operation_id, &staged)
            .await
            .map_err(Error::from)?;
        match self.transition_typed(envelope).await {
            Ok(transition) => {
                self.journal
                    .clear_outbound_envelope(&self.operation_id)
                    .await
                    .map_err(Error::from)?;
                Ok(transition)
            }
            Err(
                error @ (HostTransitionError::Rejected(_) | HostTransitionError::RebuildRequired),
            ) => {
                self.journal
                    .clear_outbound_envelope(&self.operation_id)
                    .await
                    .map_err(Error::from)?;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Replays an append-window envelope byte-for-byte after a wake.
    pub async fn drain_outbound_envelope(&self) -> Result<Option<CanonicalTransition>> {
        let Some(staged) = self
            .journal
            .read_outbound_envelope(&self.operation_id)
            .await
            .map_err(Error::from)?
        else {
            return Ok(None);
        };
        let envelope: WireEnvelope = serde_json::from_str(&staged).map_err(|error| {
            Error::Other(format!(
                "staged canonical outbound envelope is malformed: {error}"
            ))
        })?;
        match self.transition_typed(envelope).await {
            Ok(transition) => {
                self.journal
                    .clear_outbound_envelope(&self.operation_id)
                    .await
                    .map_err(Error::from)?;
                Ok(Some(transition))
            }
            Err(
                error @ (HostTransitionError::Rejected(_) | HostTransitionError::RebuildRequired),
            ) => {
                self.journal
                    .clear_outbound_envelope(&self.operation_id)
                    .await
                    .map_err(Error::from)?;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Restore the typed kernel solely from the durable journal.
    pub async fn restore(&self) -> Result<()> {
        self.restore_typed().await.map_err(Error::from)
    }

    /// Execute the install → acknowledge → reclaim checkpoint boundary.
    pub async fn checkpoint(&self) -> Result<InstalledCheckpoint> {
        self.checkpoint_typed().await.map_err(Error::from)
    }

    async fn restore_typed(&self) -> std::result::Result<(), HostTransitionError> {
        let checkpoint = self
            .journal
            .latest_checkpoint(&self.operation_id)
            .await
            .map_err(HostTransitionError::Journal)?;
        let records = self
            .journal
            .records_after(
                &self.operation_id,
                checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.covered_head.as_str()),
            )
            .await
            .map_err(HostTransitionError::Journal)?;
        self.kernel
            .lock()
            .unwrap()
            .restore_bytes(
                checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.checkpoint_bytes.as_slice()),
                &records
                    .iter()
                    .map(|record| record.record_bytes.clone())
                    .collect::<Vec<_>>(),
            )
            .map_err(HostTransitionError::Rejected)?;
        Ok(())
    }

    async fn checkpoint_typed(
        &self,
    ) -> std::result::Result<InstalledCheckpoint, HostTransitionError> {
        let candidate = self
            .kernel
            .lock()
            .unwrap()
            .checkpoint_candidate()
            .map_err(HostTransitionError::Rejected)?;
        let previous = self
            .journal
            .latest_checkpoint(&self.operation_id)
            .await
            .map_err(HostTransitionError::Journal)?;
        let checkpoint = CheckpointCandidate {
            checkpoint_id: candidate.ack_token.as_str().to_string(),
            through_step_seq: candidate.through_step_seq.get(),
            state_digest: candidate.state_digest.as_str().to_string(),
            checkpoint_bytes: candidate.checkpoint_bytes.as_slice().to_vec(),
        };
        let installed = match self
            .journal
            .compare_and_install_checkpoint(
                &self.operation_id,
                previous
                    .as_ref()
                    .map(|checkpoint| checkpoint.checkpoint_id.as_str()),
                candidate.covered_head.as_str(),
                checkpoint,
            )
            .await
        {
            Ok(installed) => installed,
            Err(error @ JournalError::CasConflict(_)) => {
                let winner = self
                    .journal
                    .latest_checkpoint(&self.operation_id)
                    .await
                    .map_err(HostTransitionError::Journal)?;
                match winner {
                    Some(winner) if winner.checkpoint_id == candidate.ack_token.as_str() => winner,
                    _ => return Err(error.into()),
                }
            }
            Err(error) => return Err(HostTransitionError::Journal(error)),
        };
        self.journal
            .ack_checkpoint(&self.operation_id, &installed.checkpoint_id)
            .await
            .map_err(HostTransitionError::Journal)?;
        self.kernel
            .lock()
            .unwrap()
            .note_checkpoint_acked(&candidate.boundary())
            .map_err(HostTransitionError::Rejected)?;
        self.journal
            .prune_acked_prefix(&self.operation_id)
            .await
            .map_err(HostTransitionError::Journal)?;
        Ok(InstalledCheckpoint {
            acknowledged: true,
            ..installed
        })
    }

    async fn transition_typed(
        &self,
        envelope: WireEnvelope,
    ) -> std::result::Result<CanonicalTransition, HostTransitionError> {
        let mut reconciliations = 0;
        loop {
            let preparation = self.kernel.lock().unwrap().prepare(&envelope);
            match preparation {
                CanonicalPreparation::Rejected(rejected)
                    if rejected.fault.code == KernelFaultCode::CheckpointRequired =>
                {
                    if reconciliations >= MAX_TRANSITION_RECONCILIATIONS {
                        return Err(HostTransitionError::Other(Error::Other(format!(
                            "canonical transition still requires a checkpoint after {reconciliations} reconciliations: {}",
                            rejected.fault.message
                        ))));
                    }
                    reconciliations += 1;
                    self.checkpoint_typed().await?;
                }
                CanonicalPreparation::Rejected(rejected) => {
                    return Err(HostTransitionError::Rejected(rejected.fault));
                }
                CanonicalPreparation::Replayed(replayed) => {
                    let planned_step = replayed.committed_step.ok_or_else(|| {
                        HostTransitionError::Other(Error::Other(
                            "canonical replay has no reproducible planned step".into(),
                        ))
                    })?;
                    return Ok(CanonicalTransition {
                        envelope,
                        step_seq: replayed.step_seq.get(),
                        record_digest: replayed.record_digest.as_str().to_string(),
                        planned_step,
                        checkpoint_advised: false,
                        replayed: true,
                    });
                }
                CanonicalPreparation::Prepared(prepared) => {
                    let record = prepared.record.clone();
                    let append = self
                        .journal
                        .compare_and_append(
                            &self.operation_id,
                            record
                                .previous_record_digest()
                                .map(|digest| digest.as_str()),
                            JournalRecordInput {
                                step_seq: record.step_seq().get(),
                                record_digest: record.record_digest().as_str().to_string(),
                                record_bytes: record.record_bytes().into_vec(),
                            },
                        )
                        .await;
                    let receipt = match append {
                        Ok(receipt) => receipt,
                        Err(error) => {
                            self.kernel
                                .lock()
                                .unwrap()
                                .abort(&prepared.token)
                                .map_err(HostTransitionError::Rejected)?;
                            if error.is_retryable() {
                                if reconciliations >= MAX_TRANSITION_RECONCILIATIONS {
                                    return Err(HostTransitionError::Journal(error));
                                }
                                reconciliations += 1;
                                self.restore_typed().await?;
                                continue;
                            }
                            return Err(HostTransitionError::Journal(error));
                        }
                    };
                    let committed: CanonicalCommit = match self
                        .kernel
                        .lock()
                        .unwrap()
                        .commit(&prepared.token, record.record_digest())
                    {
                        Ok(committed) => committed,
                        Err(_) => {
                            self.restore_typed().await?;
                            return Err(HostTransitionError::RebuildRequired);
                        }
                    };
                    if committed.step_seq.get() != receipt.step_seq
                        || committed.record.record_digest().as_str() != receipt.record_digest
                    {
                        self.restore_typed().await?;
                        return Err(HostTransitionError::RebuildRequired);
                    }
                    let checkpoint_advised = committed.checkpoint_advice.is_some();
                    let transition = CanonicalTransition {
                        envelope,
                        step_seq: committed.step_seq.get(),
                        record_digest: committed.record.record_digest().as_str().to_string(),
                        planned_step: committed.step,
                        checkpoint_advised,
                        replayed: false,
                    };
                    if checkpoint_advised {
                        self.checkpoint_typed().await?;
                    }
                    return Ok(transition);
                }
            }
        }
    }

    pub fn next_observed_at_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Build a typed envelope from a JSON wire input object (the five-class taxonomy).
    pub async fn transition_input(&self, input: Value) -> Result<CanonicalTransition> {
        self.transition_input_correlated(
            input,
            format!("rust-input-{}", uuid::Uuid::new_v4()),
            Self::next_observed_at_ms(),
        )
        .await
    }

    /// Build a typed envelope with caller-owned identity and observation time.
    ///
    /// Durable hosts use this entry point when the input already has a stable delivery identity.
    /// Retrying the same input must reuse both values so core can distinguish replay from conflict.
    pub async fn transition_input_correlated(
        &self,
        input: Value,
        input_id: impl Into<String>,
        observed_at_ms: u64,
    ) -> Result<CanonicalTransition> {
        let input: KernelInput = serde_json::from_value(input)
            .map_err(|error| Error::Other(format!("canonical wire input is malformed: {error}")))?;
        let operation_id = OperationId::new(self.operation_id.clone())
            .map_err(|error| Error::Other(error.to_string()))?;
        let input_id =
            InputId::new(input_id.into()).map_err(|error| Error::Other(error.to_string()))?;
        let envelope =
            WireEnvelope::new(operation_id, input_id, WireU64::new(observed_at_ms), input);
        self.transition(envelope).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transitions_and_replays_a_typed_canonical_envelope() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/kernel-wire/golden_lifecycle_agent_root.json"
        ))
        .expect("fixture");
        let envelope: WireEnvelope =
            serde_json::from_value(fixture["links"][0]["envelope"].clone()).expect("envelope");
        let journal: Arc<dyn KernelJournal> =
            Arc::new(crate::runtime::kernel_journal::InMemoryKernelJournal::new());
        let host = CanonicalKernelHost::new(
            CanonicalKernel::default(),
            journal,
            envelope.operation_id.as_str(),
        )
        .expect("host");

        let first = host.transition(envelope.clone()).await.expect("transition");
        assert!(!first.replayed);
        assert_eq!(first.step_seq, 0);
        assert!(host.pending_effects().is_empty());

        let replay = host.transition(envelope).await.expect("replay");
        assert!(replay.replayed);
        assert_eq!(replay.record_digest, first.record_digest);
    }

    #[tokio::test]
    async fn correlated_input_preserves_caller_identity_and_clock() {
        let journal: Arc<dyn KernelJournal> =
            Arc::new(crate::runtime::kernel_journal::InMemoryKernelJournal::new());
        let host =
            CanonicalKernelHost::new(CanonicalKernel::default(), journal, "op-correlated-input")
                .expect("host");

        let transition = host
            .transition_input_correlated(
                json!({
                    "kind": "configure_operation",
                    "config": {
                        "host_effect_support": { "supported": ["call_provider"] }
                    }
                }),
                "delivery-42",
                1_700_000_000_123,
            )
            .await
            .expect("transition");

        assert_eq!(transition.envelope.input_id.as_str(), "delivery-42");
        assert_eq!(transition.envelope.observed_at_ms.get(), 1_700_000_000_123);
    }
}
