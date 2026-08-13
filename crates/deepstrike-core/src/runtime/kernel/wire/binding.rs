//! Binding-safe façade for the canonical durable transition protocol.
//!
//! [`CanonicalKernel`] is the public pair that hosts need: the transaction decides whether an
//! envelope is accepted, while the driver decides what the accepted input means. Keeping the pair
//! behind one handle makes two invalid call sequences unavailable to bindings:
//!
//! * a host cannot call the semantic planner without first preparing a durable record;
//! * a host cannot commit the transaction without also advancing the driver's committed fold.
//!
//! There is intentionally no `step` method. Production callers must prepare, CAS-append the exact
//! core-produced record bytes, then commit. Checkpoint restore mutates the handle in place so a
//! binding object keeps its identity across a CAS rebuild.

use super::checkpoint::{
    CheckpointCandidate, KernelCheckpoint, LogicalKernelState, LogicalStateProjection,
};
use super::config::ConfigDefaults;
use super::driver::{CanonicalOperationDriver, PlannedStep};
use super::effect::{Digest, KernelEffect};
use super::envelope::{
    OperationLifecycle, WireEnvelope, WireRejection, WireRejectionKind, decode_envelope_json,
};
use super::fault::{
    KernelFault, KernelFaultCode, KernelPreparation, PrepareToken, RejectedTransition,
};
use super::record::{KernelRecord, RecordPreparation};
use super::restore::{RestoreCost, restore_operation};
use super::terminal::KernelTerminal;
use super::transaction::{
    CheckpointBoundary, CommittedTransition, DurableHead, InMemoryRecordIndex, KernelTransaction,
    TailUsage,
};

type CanonicalTransaction = KernelTransaction<PlannedStep, InMemoryRecordIndex>;

enum DriverRestorePoint {
    Fresh,
    Logical(Box<LogicalKernelState>),
}

/// One canonical operation driven exclusively through the durable transition protocol.
///
/// The type is deliberately not `Clone`: duplicating a live candidate would make two handles able
/// to commit the same prepare token. Rebuild instead uses [`Self::restore`] or
/// [`Self::restore_bytes`], both of which replace this handle's internals in place.
pub struct CanonicalKernel {
    defaults: ConfigDefaults,
    transaction: CanonicalTransaction,
    driver: CanonicalOperationDriver,
    before_candidate: Option<DriverRestorePoint>,
}

impl std::fmt::Debug for CanonicalKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanonicalKernel")
            .field("operation_id", &self.transaction.operation_id())
            .field("head", &self.transaction.head())
            .field("lifecycle", &self.transaction.lifecycle())
            .field("has_candidate", &self.transaction.has_candidate())
            .finish()
    }
}

impl Default for CanonicalKernel {
    fn default() -> Self {
        Self::new(ConfigDefaults::default())
    }
}

impl CanonicalKernel {
    /// Construct an empty operation under explicit compile-time defaults/bootstrap ceilings.
    pub fn new(defaults: ConfigDefaults) -> Self {
        Self {
            transaction: KernelTransaction::new(defaults.clone(), InMemoryRecordIndex::new()),
            driver: CanonicalOperationDriver::new(),
            defaults,
            before_candidate: None,
        }
    }

    /// Strict JSON boundary for dynamic-language bindings.
    ///
    /// Decode failures use the same closed `Rejected` arm as typed policy/lifecycle failures; a
    /// malformed payload never escapes as one language's parser exception.
    pub fn prepare_json(&mut self, input_json: &str) -> RecordPreparation<PlannedStep> {
        match decode_envelope_json(input_json, &self.defaults.bootstrap_limits) {
            Ok(envelope) => self.prepare(&envelope),
            Err(rejection) => KernelPreparation::Rejected(RejectedTransition {
                fault: rejection_fault(rejection),
            }),
        }
    }

    /// Plan one typed canonical envelope and stage its core-produced record.
    pub fn prepare(&mut self, envelope: &WireEnvelope) -> RecordPreparation<PlannedStep> {
        // Preserve an existing candidate. `KernelTransaction::prepare` will return the structured
        // transaction-conflict rejection before invoking this closure.
        if self.before_candidate.is_some() {
            let Self {
                transaction,
                driver,
                ..
            } = self;
            return transaction.prepare(envelope, |context| driver.plan(context));
        }

        let restore_point = match self.capture_driver_restore_point() {
            Ok(restore_point) => restore_point,
            Err(fault) => {
                return KernelPreparation::Rejected(RejectedTransition { fault });
            }
        };
        let preparation = {
            let Self {
                transaction,
                driver,
                ..
            } = self;
            transaction.prepare(envelope, |context| driver.plan(context))
        };

        if matches!(preparation, KernelPreparation::Prepared(_)) {
            self.before_candidate = Some(restore_point);
            return preparation;
        }

        // A planner can advance the semantic engine before a later record/tail screen rejects the
        // preparation. The transaction promises zero mutation for this arm, so restore the driver
        // to make the promise true for the pair as well.
        if let Err(fault) = self.restore_driver(restore_point) {
            return KernelPreparation::Rejected(RejectedTransition { fault });
        }
        preparation
    }

    /// Complete a transition after the host durably appended the candidate record.
    pub fn commit(
        &mut self,
        token: &PrepareToken,
        appended_head: &Digest,
    ) -> Result<CommittedTransition<PlannedStep>, KernelFault> {
        let committed = self.transaction.commit(token, appended_head)?;
        let step_seq = committed.step_seq;
        self.before_candidate = None;
        self.driver.note_committed(step_seq)?;
        Ok(committed)
    }

    /// Discard a candidate that did not reach the journal and restore the semantic driver.
    pub fn abort(&mut self, token: &PrepareToken) -> Result<KernelRecord, KernelFault> {
        let record = self.transaction.abort(token)?;
        let restore_point = self.before_candidate.take().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::TransactionConflict,
                "the transaction aborted a candidate but the canonical driver has no restore point",
            )
        })?;
        self.restore_driver(restore_point)?;
        Ok(record)
    }

    /// Rebuild this exact handle from a verified checkpoint plus records above it.
    ///
    /// `checkpoint = None` means the records are the whole retained journal and the fold starts at
    /// genesis. With a checkpoint, `records` are strictly above `through_step_seq`.
    pub fn restore(
        &mut self,
        checkpoint: Option<&KernelCheckpoint>,
        records: &[KernelRecord],
    ) -> Result<RestoreCost, KernelFault> {
        let restored = restore_operation(
            checkpoint,
            records,
            self.defaults.clone(),
            InMemoryRecordIndex::from_records(records),
        )?;
        let cost = restored.cost;
        self.transaction = restored.transaction;
        self.driver = restored.driver;
        self.before_candidate = None;
        Ok(cost)
    }

    /// Binding boundary for native checkpoint/record byte containers.
    pub fn restore_bytes(
        &mut self,
        checkpoint_bytes: Option<&[u8]>,
        record_bytes: &[Vec<u8>],
    ) -> Result<RestoreCost, KernelFault> {
        let checkpoint = checkpoint_bytes
            .map(KernelCheckpoint::from_checkpoint_bytes)
            .transpose()
            .map_err(|error| error.fault())?;
        let records = record_bytes
            .iter()
            .map(|bytes| {
                KernelRecord::from_record_bytes(bytes)
                    .map_err(|error| KernelFault::new(error.code(), error.message().to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.restore(checkpoint.as_ref(), &records)
    }

    /// Produce a full-state checkpoint candidate over the current durable head.
    pub fn checkpoint_candidate(&self) -> Result<CheckpointCandidate, KernelFault> {
        self.transaction
            .checkpoint_candidate(self.driver.project_logical_state())
    }

    /// Produce the incremental checkpoint form over a previously captured logical base.
    pub fn checkpoint_rebase(
        &self,
        base: &KernelCheckpoint,
    ) -> Result<CheckpointCandidate, KernelFault> {
        self.transaction
            .checkpoint_rebase(&base.boundary(), base.logical_state().clone())
    }

    /// Close the retention boundary after the host durably acknowledged a checkpoint install.
    pub fn note_checkpoint_acked(
        &mut self,
        boundary: &CheckpointBoundary,
    ) -> Result<TailUsage, KernelFault> {
        self.transaction.note_checkpoint_acked(boundary)
    }

    pub fn head(&self) -> Option<DurableHead> {
        self.transaction.head()
    }

    pub fn lifecycle(&self) -> OperationLifecycle {
        self.transaction.lifecycle()
    }

    pub fn pending_effects(&self) -> impl Iterator<Item = &KernelEffect> {
        self.transaction.pending_effects()
    }

    pub fn terminal(&self) -> Option<&KernelTerminal> {
        self.transaction.terminal()
    }

    /// Return the live kernel-issued attempt for a child task, including after checkpoint restore.
    pub fn attempt_id(&self, task_id: &str) -> Option<super::scalar::AttemptId> {
        self.driver.attempt_id(task_id).cloned()
    }

    /// Current scheduler turn, restored from the canonical journal/checkpoint state.
    pub fn turn(&self) -> u32 {
        self.driver.engine().map_or(0, |engine| engine.turn)
    }

    /// Recovery replay budget in bytes, when the operation has been configured.
    pub fn recovery_content_bytes(&self) -> Option<usize> {
        self.driver.engine().map(|engine| {
            let tokens = engine
                .ctx
                .config
                .recovery_content_tokens(engine.ctx.max_tokens);
            engine.ctx.engine.token_budget_to_bytes(tokens)
        })
    }

    /// Task-state references that context pressure must keep resident.
    pub fn preserved_refs(&self) -> Vec<String> {
        self.driver
            .engine()
            .map(|engine| engine.ctx.partitions.task_state.preserved_refs.clone())
            .unwrap_or_default()
    }

    /// Count text using the configured canonical context token engine.
    pub fn count_tokens(&self, text: &str) -> Option<u32> {
        self.driver
            .engine()
            .map(|engine| engine.ctx.engine.count(text))
    }

    /// Cumulative kernel-owned child spawn count for this operation.
    pub fn local_subagents_spawned(&self) -> u32 {
        self.driver
            .engine()
            .map_or(0, |engine| engine.local_subagents_spawned())
    }

    /// Messages added to the canonical operation history.
    pub fn new_messages(&self) -> Vec<crate::types::message::Message> {
        self.driver
            .engine()
            .map(|engine| engine.drain_new_messages())
            .unwrap_or_default()
    }

    pub fn poison(&self) -> Option<&KernelFault> {
        self.transaction.poison().or_else(|| self.driver.poison())
    }

    fn capture_driver_restore_point(&self) -> Result<DriverRestorePoint, KernelFault> {
        if self.transaction.config().is_none() {
            return Ok(DriverRestorePoint::Fresh);
        }
        let LogicalStateProjection {
            root_kind,
            focus,
            syscall,
            scheduler,
            context_vm,
        } = self.driver.project_logical_state();
        let transition = self
            .transaction
            .transition_state_for_restore(root_kind, focus)?;
        Ok(DriverRestorePoint::Logical(Box::new(LogicalKernelState {
            transition,
            syscall,
            scheduler,
            context_vm,
        })))
    }

    fn restore_driver(&mut self, restore_point: DriverRestorePoint) -> Result<(), KernelFault> {
        self.driver = match restore_point {
            DriverRestorePoint::Fresh => CanonicalOperationDriver::new(),
            DriverRestorePoint::Logical(state) => CanonicalOperationDriver::restore_logical_state(
                &state.transition.resolved_config,
                &state,
            )?,
        };
        Ok(())
    }
}

fn rejection_fault(rejection: WireRejection) -> KernelFault {
    let code = match rejection.kind {
        WireRejectionKind::PolicyViolation => KernelFaultCode::InvalidConfig,
        _ => KernelFaultCode::MalformedEnvelope,
    };
    KernelFault::new(code, rejection.message)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::CanonicalKernel;
    use crate::runtime::kernel::wire::{
        KernelFaultCode, KernelPreparation, PrepareToken, WireEnvelope,
    };

    fn golden_agent_root() -> Value {
        serde_json::from_str(include_str!(
            "../../../../../../tests/fixtures/kernel-wire/golden_lifecycle_agent_root.json"
        ))
        .expect("golden fixture")
    }

    fn commit_input(kernel: &mut CanonicalKernel, input: &str) {
        let prepared = kernel.prepare_json(input);
        let KernelPreparation::Prepared(prepared) = prepared else {
            panic!("expected prepared transition");
        };
        kernel
            .commit(&prepared.token, prepared.record.record_digest())
            .expect("commit");
    }

    #[test]
    fn canonical_kernel_produces_the_shared_genesis_record() {
        let fixture = golden_agent_root();
        let mut kernel = CanonicalKernel::default();
        let preparation = kernel.prepare_json(&fixture["links"][0]["envelope"].to_string());
        let KernelPreparation::Prepared(prepared) = preparation else {
            panic!("golden envelope must prepare");
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

    #[test]
    fn abort_restores_the_driver_before_the_next_prepare() {
        let fixture = golden_agent_root();
        let mut kernel = CanonicalKernel::default();
        commit_input(&mut kernel, &fixture["links"][0]["envelope"].to_string());

        let start = fixture["links"][1]["envelope"].clone();
        let first = kernel.prepare_json(&start.to_string());
        let KernelPreparation::Prepared(first) = first else {
            panic!("start must prepare");
        };
        let first_digest = first.record.record_digest().clone();
        kernel.abort(&first.token).expect("abort before append");

        let second = kernel.prepare_json(&start.to_string());
        let KernelPreparation::Prepared(second) = second else {
            panic!("the same input must prepare after abort");
        };
        assert_eq!(second.record.record_digest(), &first_digest);
    }

    #[test]
    fn malformed_and_unknown_envelopes_are_structured_rejections() {
        let mut kernel = CanonicalKernel::default();
        let malformed = kernel.prepare_json("{");
        assert_eq!(
            malformed.fault().map(|fault| fault.code),
            Some(KernelFaultCode::MalformedEnvelope)
        );

        let fixture = golden_agent_root();
        let mut unknown = fixture["links"][0]["envelope"].clone();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("session_id".to_string(), Value::String("host-only".into()));
        let rejected = kernel.prepare_json(&unknown.to_string());
        assert_eq!(
            rejected.fault().map(|fault| fault.code),
            Some(KernelFaultCode::MalformedEnvelope)
        );
    }

    #[test]
    fn restore_replaces_the_same_typed_handle() {
        let fixture = golden_agent_root();
        let mut kernel = CanonicalKernel::default();
        commit_input(&mut kernel, &fixture["links"][0]["envelope"].to_string());
        let checkpoint = kernel
            .checkpoint_candidate()
            .expect("configured operation checkpoints")
            .decode()
            .expect("checkpoint verifies");

        let start: WireEnvelope =
            serde_json::from_value(fixture["links"][1]["envelope"].clone()).unwrap();
        let KernelPreparation::Prepared(prepared) = kernel.prepare(&start) else {
            panic!("start prepares");
        };
        let post_checkpoint_record = prepared.record.clone();
        let expected_head = prepared.record.record_digest().clone();
        kernel
            .commit(&prepared.token, prepared.record.record_digest())
            .unwrap();

        let handle_address = std::ptr::addr_of!(kernel);
        kernel
            .restore(Some(&checkpoint), &[post_checkpoint_record])
            .expect("restore");
        assert_eq!(std::ptr::addr_of!(kernel), handle_address);
        assert_eq!(kernel.head().unwrap().digest, expected_head);

        let no_candidate = PrepareToken::new("no-candidate").unwrap();
        assert!(kernel.abort(&no_candidate).is_err());
    }
}
