use super::LoopStateMachine;
use crate::runtime::kernel::KernelObservation;
use crate::runtime::kernel::wire::CancellationReason;
use crate::scheduler::state_machine::LoopAction;
use crate::scheduler::tcb::TaskLifecycle;
use crate::types::result::TerminationReason;

impl LoopStateMachine {
    /// Commit a host-owned cancellation after external I/O has already been stopped.
    pub fn cancel_operation(
        &mut self,
        operation_id: String,
        reason: CancellationReason,
        pending_call_ids: Vec<String>,
    ) -> LoopAction {
        self.observations.clear();

        let mut child_ids = self
            .tasks
            .all()
            .iter()
            .filter(|task| task.id.as_str() != "root" && !task.state.is_terminal())
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        child_ids.sort_by(|left, right| {
            self.tasks
                .lineage_depth(right.as_str())
                .cmp(&self.tasks.lineage_depth(left.as_str()))
        });
        for child_id in child_ids {
            if let Some(task) = self.tasks.get_mut(child_id.as_str()) {
                task.state = TaskLifecycle::Done(TerminationReason::UserAbort);
            }
            // spc_005-05: no join result exists for an abrupt cancellation, so `consumed` stays
            // whatever the grant already held (zero) — the full reservation returns to the parent.
            self.tasks.return_child_budget(child_id.as_str());
            // spc_003-04: `set_wait` keeps `WaitIndex` in sync — it is the only sanctioned mutator.
            self.tasks.clear_wait(child_id.as_str());
        }

        self.suspend_state = None;
        self.pending_denied_results.clear();
        self.workflow = None;
        self.pending_workflow_spawn = None;
        self.pending_preempt = None;
        self.pending_host_effects.clear();
        self.active_host_effect = None;
        self.deferred_action = None;
        self.pending_termination = None;
        self.pending_pace = None;

        self.observations
            .push(KernelObservation::OperationCancelled {
                turn: self.turn,
                operation_id,
                reason,
                pending_call_ids,
            });
        self.terminate(TerminationReason::UserAbort, None)
    }
}
