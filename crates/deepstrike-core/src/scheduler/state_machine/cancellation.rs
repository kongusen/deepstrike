use super::LoopStateMachine;
use crate::runtime::kernel::{CancellationReason, KernelObservation};
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

        let child_ids = self
            .tasks
            .all()
            .iter()
            .filter(|task| task.id.as_str() != "root" && !task.state.is_terminal())
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        for child_id in child_ids {
            if let Some(task) = self.tasks.get_mut(child_id.as_str()) {
                task.state = TaskLifecycle::Done(TerminationReason::UserAbort);
                task.wait = None;
            }
        }

        self.suspend_state = None;
        self.pending_denied_results.clear();
        self.workflow = None;
        self.pending_workflow_spawn = None;
        self.pending_preempt = None;
        self.pending_host_effects.clear();
        self.active_host_effect = None;
        self.active_host_effect_failures = 0;
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
