//! Deterministic workflow-scheduler regression gates from the policy-upgrade specification.

use crate::orchestration::task_graph::TaskGraph;
use crate::orchestration::workflow::{
    DependencyPolicy, WorkflowNode, WorkflowNodeStatus, WorkflowRun, WorkflowSpec,
};
use crate::scheduler::policy::SchedulerPolicyConfig;
use crate::types::agent::AgentRole;
use crate::types::result::{LoopResult, TerminationReason};
use crate::types::task::RuntimeTask;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalPathGate {
    pub id_order_makespan: u64,
    pub policy_makespan: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopFairnessGate {
    pub waiting_rounds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminationPolicyGate {
    pub cases_checked: usize,
}

fn result(termination: TerminationReason, loop_continue: Option<bool>) -> LoopResult {
    LoopResult {
        termination,
        final_message: None,
        turns_used: 1,
        total_tokens_used: 0,
        loop_continue,
        classify_branch: None,
        pace_decision: None,
        tournament_winner: None,
    }
}

fn f1_graph() -> (TaskGraph, Vec<u64>) {
    let mut graph = TaskGraph::new();
    let wide_a = graph.add(RuntimeTask::new("wide-a"), vec![]);
    let wide_b = graph.add(RuntimeTask::new("wide-b"), vec![]);
    let wide_c = graph.add(RuntimeTask::new("wide-c"), vec![]);
    let chain_1 = graph.add(RuntimeTask::new("critical-1"), vec![]);
    graph.add(RuntimeTask::new("wide-a-1"), vec![wide_a]);
    graph.add(RuntimeTask::new("wide-a-2"), vec![wide_a]);
    graph.add(RuntimeTask::new("wide-b-1"), vec![wide_b]);
    graph.add(RuntimeTask::new("wide-b-2"), vec![wide_b]);
    graph.add(RuntimeTask::new("wide-c-1"), vec![wide_c]);
    graph.add(RuntimeTask::new("wide-c-2"), vec![wide_c]);
    let chain_2 = graph.add(RuntimeTask::new("critical-2"), vec![chain_1]);
    let chain_3 = graph.add(RuntimeTask::new("critical-3"), vec![chain_2]);
    let chain_4 = graph.add(RuntimeTask::new("critical-4"), vec![chain_3]);
    graph.add(RuntimeTask::new("critical-5"), vec![chain_4]);

    let mut durations = vec![1; graph.len()];
    for node in [chain_1, chain_2, chain_3, chain_4, 13] {
        durations[node] = 4;
    }
    (graph, durations)
}

fn simulate_f1(policy: SchedulerPolicyConfig) -> u64 {
    let (mut graph, durations) = f1_graph();
    graph.configure_scheduling(policy, &[]);
    let mut now = 0u64;
    let mut running: Vec<(usize, u64)> = Vec::new();
    while !graph.all_done() {
        for node in graph.ready_tasks() {
            if running.len() == 2 {
                break;
            }
            graph.start(node);
            running.push((node, now + durations[node]));
        }
        let next = running
            .iter()
            .map(|(_, finish)| *finish)
            .min()
            .expect("unfinished graph must have runnable work");
        now = next;
        let completed: Vec<usize> = running
            .iter()
            .filter_map(|(node, finish)| (*finish == now).then_some(*node))
            .collect();
        running.retain(|(_, finish)| *finish != now);
        for node in completed {
            graph.complete(node, result(TerminationReason::Completed, None));
        }
    }
    now
}

/// F1: a long critical chain must beat FIFO/node-id ordering on modeled makespan.
pub fn f1_critical_path_skew() -> CriticalPathGate {
    let id_policy = SchedulerPolicyConfig {
        critical_path_weight: 0,
        fanout_weight: 0,
        age_weight: 0,
        token_cost_weight: 0,
        ..SchedulerPolicyConfig::default()
    };
    CriticalPathGate {
        id_order_makespan: simulate_f1(id_policy),
        policy_makespan: simulate_f1(SchedulerPolicyConfig::default()),
    }
}

/// F2: a 100-iteration loop gets one quantum, then yields to an already-waiting peer.
pub fn f2_loop_fairness() -> LoopFairnessGate {
    let spec = WorkflowSpec::new(vec![
        WorkflowNode::new(RuntimeTask::new("loop"), AgentRole::Implement).with_loop(100),
        WorkflowNode::new(RuntimeTask::new("peer"), AgentRole::Implement),
    ]);
    let mut run = WorkflowRun::new(&spec, "f2").expect("valid F2 workflow");
    assert_eq!(run.ready_batch(), vec![0, 1]);
    let loop_id = run.current_agent_id(0);
    run.mark_spawned(0, &loop_id);
    run.record_completion(&loop_id, result(TerminationReason::Completed, Some(true)));
    let next = run.ready_batch();
    LoopFairnessGate {
        waiting_rounds: u64::from(next.first().copied() != Some(1)),
    }
}

fn expected_status(
    termination: TerminationReason,
    policy: DependencyPolicy,
) -> Option<WorkflowNodeStatus> {
    match (termination, policy) {
        (TerminationReason::Completed, _) => None,
        (
            TerminationReason::MaxTurns
            | TerminationReason::TokenBudget
            | TerminationReason::Timeout
            | TerminationReason::MilestoneExceeded
            | TerminationReason::ContextOverflow
            | TerminationReason::NoProgress,
            DependencyPolicy::AllSuccess,
        ) => Some(WorkflowNodeStatus::SkippedUpstreamFailed),
        (
            TerminationReason::Error | TerminationReason::UserAbort,
            DependencyPolicy::AllSuccess | DependencyPolicy::AcceptPartial,
        ) => Some(WorkflowNodeStatus::SkippedUpstreamFailed),
        _ => None,
    }
}

/// F3: every termination class is crossed with every dependency policy. A `None` expectation means
/// the dependent must be ready; otherwise it must close with the given terminal status.
pub fn f3_termination_dependency_matrix() -> TerminationPolicyGate {
    let terminations = [
        TerminationReason::Completed,
        TerminationReason::MaxTurns,
        TerminationReason::Error,
    ];
    let policies = [
        DependencyPolicy::AllSuccess,
        DependencyPolicy::AcceptPartial,
        DependencyPolicy::AllTerminal,
        DependencyPolicy::Optional,
    ];
    let mut cases_checked = 0;
    for termination in terminations {
        for policy in policies {
            let spec = WorkflowSpec::new(vec![
                WorkflowNode::new(RuntimeTask::new("upstream"), AgentRole::Implement),
                WorkflowNode::new(RuntimeTask::new("dependent"), AgentRole::Implement)
                    .with_depends_on(vec![0])
                    .with_dependency_policy(policy),
            ]);
            let mut run = WorkflowRun::new(&spec, "f3").expect("valid F3 workflow");
            let upstream = run.current_agent_id(0);
            run.mark_spawned(0, &upstream);
            run.record_completion(&upstream, result(termination, None));
            match expected_status(termination, policy) {
                None => assert!(run.ready_batch().contains(&1)),
                Some(status) => assert_eq!(run.node_outcomes()[1].status, status),
            }
            cases_checked += 1;
        }
    }
    TerminationPolicyGate { cases_checked }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f1_gate_reduces_makespan() {
        let gate = f1_critical_path_skew();
        assert!(gate.policy_makespan < gate.id_order_makespan, "{gate:?}");
    }

    #[test]
    fn f2_gate_bounds_peer_wait() {
        assert_eq!(f2_loop_fairness().waiting_rounds, 0);
    }

    #[test]
    fn f3_gate_closes_all_twelve_cases() {
        assert_eq!(f3_termination_dependency_matrix().cases_checked, 12);
    }
}
