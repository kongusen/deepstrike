//! Signal routing impl for [`super::LoopStateMachine`].

use super::{KernelObservation, LoopAction, LoopPhase, LoopStateMachine, SuspendState};
use crate::signals::router::SignalRouter;
use crate::types::policy::SignalDisposition;
use crate::types::result::TerminationReason;
use crate::types::signal::RuntimeSignal;

use super::super::tcb::TaskLifecycle;

/// Stable snake_case label for a signal disposition, used in `SignalDisposed`
/// observations (part of the observation wire format).
pub(super) fn disposition_label(d: &SignalDisposition) -> &'static str {
    match d {
        SignalDisposition::Ignore => "ignore",
        SignalDisposition::Observe => "observe",
        SignalDisposition::Queue => "queue",
        SignalDisposition::Run { .. } => "run",
        SignalDisposition::Interrupt => "interrupt",
        SignalDisposition::InterruptNow => "interrupt_now",
        SignalDisposition::Dropped => "dropped",
    }
}

impl LoopStateMachine {
    /// Enable in-kernel signal routing with the default urgency-based attention
    /// policy and a bounded queue. Once set, inbound signals are dispatched through
    /// the kernel (dedup + disposition + queue) instead of the legacy `feed` path.
    pub fn set_attention(&mut self, max_queue_size: usize) {
        self.signal_router = SignalRouter::new(max_queue_size);
    }

    /// ABI entry for an inbound signal: clears observations, sweeps leases, then
    /// dispatches through the in-kernel router (or the legacy path). Returns
    /// `None` when the signal does not drive a provider call this step
    /// (queued / observed / ignored / dropped).
    pub fn signal_event(&mut self, signal: RuntimeSignal) -> Option<LoopAction> {
        self.observations.clear();
        self.sweep_expired_leases();
        // K3: skill leases expire on the same head-of-event cadence as capability leases.
        self.ctx.sweep_expired_skill_leases(self.turn);
        self.dispatch_signal(signal)
    }

    /// Route a signal and decide whether it drives a turn now. Assumes the caller
    /// has already cleared observations / swept leases (see `feed` and `signal_event`).
    pub(super) fn dispatch_signal(&mut self, signal: RuntimeSignal) -> Option<LoopAction> {
        let is_running = !matches!(self.lifecycle(), TaskLifecycle::Ready | TaskLifecycle::Done(_));
        let router = &mut self.signal_router;
        let signal_id = signal.id.to_string();
        let summary = signal.summary.to_string();
        let disposition = router.ingest(signal, is_running);
        let queue_depth = router.depth() as u32;
        self.observations.push(KernelObservation::SignalDisposed {
            turn: self.turn,
            signal_id,
            disposition: disposition_label(&disposition).to_string(),
            queue_depth,
        });
        // Acted-on external signals are user/agent directives: also promote into the durable
        // directive channel so they survive compaction/renewal (the ephemeral signal copy below is
        // cleared at the next sprint boundary). Queue/Ignore/Dropped are not acted on → not durable.
        match disposition {
            // #2-A/B: hard preempt (Critical while busy). Stop in-flight work NOW and reason about the
            // interrupt this turn. When the root is suspended awaiting running sub-agents/workflow,
            // `preempt_running_for_interrupt` aborts them (emits `AgentPreempted`) and clears the
            // suspend before we force the turn; otherwise it's a plain forced reason turn.
            SignalDisposition::InterruptNow => {
                self.ctx.record_directive(summary.clone());
                self.ctx.push_signal(format!("[INTERRUPT] {summary}"));
                self.preempt_running_for_interrupt(&summary);
                self.phase = LoopPhase::Reason;
                Some(self.emit_call_llm())
            }
            // #2-A: soft interrupt (High while busy) — record the directive so the agent handles it at
            // the NEXT turn boundary (when running children complete and the root resumes). Does NOT
            // force a turn or abort in-flight work — that distinction is `InterruptNow`'s alone.
            SignalDisposition::Interrupt => {
                self.ctx.record_directive(summary.clone());
                self.ctx.push_signal(format!("[SIGNAL] {summary}"));
                None
            }
            SignalDisposition::Run { .. } => {
                self.ctx.record_directive(summary.clone());
                self.ctx.push_signal(format!("[SIGNAL] {summary}"));
                self.phase = LoopPhase::Reason;
                Some(self.emit_call_llm())
            }
            // Observe: note it in context but don't force a turn.
            SignalDisposition::Observe => {
                self.ctx.record_directive(summary.clone());
                self.ctx.push_signal(format!("[SIGNAL] {summary}"));
                None
            }
            // Queued in the kernel (drained at the next turn boundary), or
            // deduped / dropped — no provider call this step.
            SignalDisposition::Queue
            | SignalDisposition::Ignore
            | SignalDisposition::Dropped => None,
        }
    }

    /// #2-B: when an `InterruptNow` arrives while the root is suspended awaiting running sub-agents /
    /// workflow nodes, abort them — mark each `Done(UserAbort)` (so a late real completion is a
    /// no-op), tear down an owning workflow whole (§6.1a: every non-completed node aborts → terminal
    /// `WorkflowCompleted`), emit `AgentPreempted` (the SDK aborts the in-flight runs + discards their
    /// results), and clear the suspend so the forced reason turn reclaims the root. No-op when not
    /// awaiting sub-agents (then `InterruptNow` is just a plain forced reason turn).
    pub(super) fn preempt_running_for_interrupt(&mut self, reason: &str) {
        let Some(SuspendState::SubAgentAwait { agent_ids }) = self.suspend_state.as_ref() else {
            return;
        };
        let agent_ids: Vec<String> = agent_ids.clone();
        if agent_ids.is_empty() {
            return;
        }

        // Mark each preempted child terminal (UserAbort); rebuild its `AgentProcess` view row.
        for id in &agent_ids {
            let process = if let Some(task) = self.tasks.get_mut(id.as_str()) {
                task.state = TaskLifecycle::Done(TerminationReason::UserAbort);
                crate::proc::AgentProcess::from_tcb(task)
            } else {
                None
            };
            if let Some(process) = process {
                self.push_agent_process_changed(process);
            }
        }

        // §6.1a: an owning workflow is torn down whole — every non-completed node aborts.
        if self
            .workflow
            .as_ref()
            .is_some_and(|w| agent_ids.iter().any(|id| w.owns_agent(id)))
        {
            if let Some(run) = self.workflow.take() {
                let (completed, failed) = run.abort_outcome();
                self.observations.push(KernelObservation::WorkflowCompleted {
                    turn: self.turn,
                    completed,
                    failed,
                });
            }
        }

        self.observations.push(KernelObservation::AgentPreempted {
            turn: self.turn,
            agent_ids,
            reason: reason.to_string(),
        });
        self.suspend_state = None;
    }

    /// Drain all kernel-queued signals into the current context as runtime notes.
    /// Called at turn boundaries.
    pub(super) fn drain_queued_signals(&mut self) {
        let mut out = Vec::new();
        let router = &mut self.signal_router;
        while let Some(sig) = router.next() {
            out.push(sig.summary.to_string());
        }
        for summary in out {
            self.ctx.push_signal(format!("[SIGNAL] {summary}"));
        }
    }
}
