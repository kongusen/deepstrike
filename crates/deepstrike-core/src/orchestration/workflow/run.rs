//! W0: a kernel-resident workflow run — the DAG state for one in-flight [`WorkflowSpec`].
//!
//! Pure data + pure advance logic, no I/O and no syscall: the [`crate::scheduler::state_machine::
//! LoopStateMachine`] drives this, gating each ready node's spawn through
//! `evaluate_syscall(Syscall::Spawn)` and reusing the existing batch-await barrier
//! (`SuspendState::SubAgentAwait`). This module only tracks *which* nodes are ready, spawned,
//! complete, partially complete, failed, or skipped, and builds each node's [`IsolationManifest`].
//!
//! Lifecycle: `ready_batch()` → (gate each) `mark_spawned` / `mark_denied` → on completion
//! `record_completion` → repeat until `is_complete()`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{DependencyPolicy, NodeKind, NodeTrust, WorkflowNode, WorkflowSpec};
use crate::orchestration::task_graph::{TaskGraph, TaskStatus};
use crate::orchestration::tournament::{EntrantId, Match, Tournament, TournamentAction};
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
use crate::types::error::DeepStrikeError;
use crate::types::error::Result;
use crate::types::result::{LoopResult, TerminationReason};
use crate::types::task::RuntimeTask;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSubmissionError {
    pub node_index: usize,
    pub reason: String,
}

impl std::fmt::Display for WorkflowSubmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "node {}: {}", self.node_index, self.reason)
    }
}

impl std::error::Error for WorkflowSubmissionError {}

/// Terminal state of one workflow node. Non-terminal graph states are never exposed as outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeStatus {
    Completed,
    CompletedPartial,
    Failed,
    SkippedUpstreamFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNodeOutcome {
    pub node_id: String,
    pub status: WorkflowNodeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<crate::types::message::Message>,
}

/// Deterministic kernel agent id for a workflow node (stable across resume / audit).
pub fn node_agent_id(node: usize) -> String {
    format!("wf-node{node}")
}

/// Parse a loop-iteration agent id `wf-node{N}-i{k}` into `(N, k)`; `None` for plain
/// node ids, malformed ids, or an out-of-range node index.
fn parse_loop_iteration_id(id: &str, n_nodes: usize) -> Option<(usize, usize)> {
    let rest = id.strip_prefix("wf-node")?;
    let (node_s, k_s) = rest.split_once("-i")?;
    let node: usize = node_s.parse().ok()?;
    let k: usize = k_s.parse().ok()?;
    (node < n_nodes).then_some((node, k))
}

/// Enough to run one spawned workflow node, carried to the SDK in the `WorkflowBatchSpawned`
/// observation. Role/isolation/inheritance are canonical snake_case strings (serde names) so the
/// host SDK can rebuild an agent run spec — the kernel generates these specs internally, so this
/// is how the goal reaches the SDK that actually executes the node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSpawnInfo {
    pub agent_id: String,
    pub goal: String,
    pub role: String,
    pub isolation: String,
    pub context_inheritance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<String>,
    /// W3 trust level (`"trusted"` | `"quarantined"`) — the SDK runs quarantined nodes without
    /// privileges and crosses their output back only as a structured summary.
    #[serde(default = "default_trust")]
    pub trust: String,
    /// G3 structured output: the JSON Schema the node's output must conform to, carried verbatim
    /// from [`WorkflowNode::output_schema`]. The SDK instructs the agent with it and validates +
    /// retries on its result. `None` when the node declared no schema. Additive ABI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// G2 deterministic compute: present only for a [`NodeKind::Reduce`] node — the name of the
    /// SDK-registered pure function the SDK runs (over `input_agent_ids`' outputs) instead of an LLM
    /// agent. `None` for every ordinary node. Additive ABI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reducer: Option<String>,
    /// G2: the dependency agent ids whose outputs a [`NodeKind::Reduce`] node consumes (its
    /// `depends_on`, resolved to stable agent ids). Empty for non-reduce nodes. Additive ABI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_agent_ids: Vec<String>,
    /// Present only for a tournament *judge* spawn (A#2): the two entrant agent ids whose outputs
    /// this judge must compare. The SDK looks up those entrants' produced candidates, runs the
    /// judge, and reports the winner in the result's `tournament_winner`. `None` for every ordinary
    /// (entrant / spawn / loop / classify) node. Additive ABI: omitted on the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_match: Option<JudgeMatch>,
    /// Present only for a [`NodeKind::Loop`] iteration spawn (A#2 v2): the loop's `max_iters`. It
    /// both *marks* the spawn as a loop iteration — so the SDK knows to solicit and report a
    /// `loop_continue` stop signal from the agent — and gives the cap for the agent's prompt. `None`
    /// for every non-loop node. Mirrors how `reducer` / `judge_match` distinguish reduce / judge
    /// spawns. Additive ABI: omitted on the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_max_iters: Option<usize>,
    /// Present only for a [`NodeKind::Classify`] spawn (A#2): the branch labels the classifier must
    /// choose among. Non-empty *marks* the spawn as a classifier — the SDK instructs the agent to
    /// pick exactly one label and reports it in the result's `classify_branch`. Empty for every
    /// non-classify node. Additive ABI: omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classify_labels: Vec<String>,
    /// M4/G5: the node's per-node cumulative token cap, if set. The SDK sets the child run's
    /// `max_total_tokens` to this so the node self-terminates at the cap. Additive ABI: omitted when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    /// O3: per-node turn cap → the child run's `max_turns`. Additive ABI: omitted when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// O3: per-node wall-clock cap (ms) → the child run's timeout. Additive ABI: omitted when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_ms: Option<u64>,
}

fn default_trust() -> String {
    "trusted".to_string()
}

/// A pairwise judge assignment carried to the SDK on a tournament judge's `WorkflowSpawnInfo`:
/// the two entrant agent ids whose produced outputs are to be compared. The SDK maps each id back
/// to that entrant's candidate and asks the judge which is better.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JudgeMatch {
    pub left: String,
    pub right: String,
}

/// G4 budget-as-signal: a snapshot of the workflow's remaining headroom under the active resource
/// quota, carried to the SDK on every `WorkflowBatchSpawned`. A coordinator/submitter node reads it
/// to *scale its next submission to what is actually available* — the analogue of the host-side
/// `budget.remaining()` in the code-orchestration model — instead of blindly hitting the cap and
/// eating a `Deny`. `None` remaining fields mean that dimension is unbounded (no quota set).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkflowBudget {
    /// Nodes currently in the DAG (spec + every runtime submission so far).
    pub nodes_used: usize,
    /// `ResourceQuota::max_workflow_nodes`, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes_max: Option<usize>,
    /// `nodes_max - nodes_used` (saturating), if a node cap is set — how many more nodes may be
    /// submitted before the `max_workflow_nodes` backstop denies further growth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes_remaining: Option<usize>,
    /// Sub-agents currently in the `running` state.
    pub running_subagents: usize,
    /// `ResourceQuota::max_concurrent_subagents`, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_subagents: Option<usize>,
    /// `max_concurrent_subagents - running_subagents` (saturating), if a concurrency cap is set —
    /// how many of a submission's nodes can spawn *immediately* rather than deferring for a slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_remaining: Option<usize>,
    /// M4/G5: cumulative tokens spent across the run so far (the scheduler's `total_tokens`).
    /// `#[serde(default)]` keeps older JSON (without this field) deserializing to 0 — additive ABI.
    #[serde(default)]
    pub tokens_used: u64,
    /// M4/G5: `SchedulerBudget::max_total_tokens` — the run's cumulative token cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_max: Option<u64>,
    /// M4/G5: `tokens_max - tokens_used` (saturating) — how many tokens remain before the run-level
    /// token budget terminates the workflow. Lets a coordinator scale its next submission to token
    /// headroom (the analogue of "use 10k tokens").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_remaining: Option<u64>,
}

fn role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Explore => "explore",
        AgentRole::Plan => "plan",
        AgentRole::Implement => "implement",
        AgentRole::Verify => "verify",
        AgentRole::Custom => "custom",
    }
}

fn isolation_label(isolation: AgentIsolation) -> &'static str {
    match isolation {
        AgentIsolation::Shared => "shared",
        AgentIsolation::ReadOnly => "read_only",
        AgentIsolation::Worktree => "worktree",
        AgentIsolation::Remote => "remote",
    }
}

fn inheritance_label(inheritance: ContextInheritance) -> &'static str {
    match inheritance {
        ContextInheritance::None => "none",
        ContextInheritance::SystemOnly => "system_only",
        ContextInheritance::Full => "full",
    }
}

fn trust_label(trust: NodeTrust) -> &'static str {
    match trust {
        NodeTrust::Trusted => "trusted",
        NodeTrust::Quarantined => "quarantined",
    }
}

/// Synthetic terminal result for an inert reconstructed placeholder.
fn resumed_placeholder_result() -> LoopResult {
    LoopResult {
        termination: crate::types::result::TerminationReason::Completed,
        final_message: None,
        turns_used: 0,
        total_tokens_used: 0,
        loop_continue: None,
        classify_branch: None,
        tournament_winner: None,
        pace_decision: None,
    }
}

/// One typed recovered node outcome for [`WorkflowRun::resume`], plus the result-borne control
/// signals required to reconstruct classify/loop/tournament state exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumedNodeOutcome {
    pub agent_id: String,
    pub status: WorkflowNodeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<crate::types::message::Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classify_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tournament_winner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_continue: Option<bool>,
}

impl ResumedNodeOutcome {
    pub fn completed(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            status: WorkflowNodeStatus::Completed,
            termination: Some(TerminationReason::Completed),
            output: None,
            classify_branch: None,
            tournament_winner: None,
            loop_continue: None,
        }
    }
}

/// In-flight bracket state for one `NodeKind::Tournament` controller node. Entrant and judge
/// children are appended as ordinary graph nodes (so they flow through the unchanged spawn loop);
/// this just tracks the phase and the current round's judges so completions advance the bracket.
struct TournamentState {
    /// Entrant child node indices (the generators), in entrant order.
    entrant_nodes: Vec<usize>,
    /// Entrants still generating; the bracket starts when this reaches 0.
    entrants_remaining: usize,
    /// Single-elimination bracket — `None` during the entrant phase, `Some` once judging begins.
    bracket: Option<Tournament>,
    /// Current round's judge child node indices, aligned to the bracket's pending matches.
    judge_nodes: Vec<usize>,
    /// Winner reported per current-round match (aligned to `judge_nodes`); `None` until judged.
    judge_winners: Vec<Option<EntrantId>>,
    /// Judges still deliberating this round; the round resolves when this reaches 0.
    judges_remaining: usize,
}

/// The state of one in-flight workflow execution.
pub struct WorkflowRun {
    graph: TaskGraph,
    nodes: Vec<WorkflowNode>,
    scheduler_policy: crate::scheduler::policy::SchedulerPolicyConfig,
    /// Parent session id stamped onto each node's spawned-agent manifest.
    parent_session_id: String,
    /// Completed-event lookup: kernel agent id → DAG node index.
    node_of_agent: HashMap<String, usize>,
    /// Completed-iteration count per `Loop` node (absent / 0 = no iterations finished yet). The
    /// in-flight iteration's agent id is `wf-node{N}-i{iter_counts[N]}`.
    iter_counts: HashMap<usize, usize>,
    /// In-flight bracket state per `NodeKind::Tournament` controller node index.
    tournaments: HashMap<usize, TournamentState>,
    /// Reverse map: an appended entrant/judge child node index → its controller node index.
    child_controller: HashMap<usize, usize>,
    /// Judge-match descriptor per judge child node index (read by `spawn_info`).
    judge_matches: HashMap<usize, JudgeMatch>,
}

impl WorkflowRun {
    /// Build from a spec. Validates dependency indices + acyclicity (reuses `WorkflowSpec`).
    pub fn new(spec: &WorkflowSpec, parent_session_id: &str) -> Result<Self> {
        let mut run = Self {
            graph: spec.validate()?,
            nodes: spec.nodes.clone(),
            scheduler_policy: crate::scheduler::policy::SchedulerPolicyConfig::default(),
            parent_session_id: parent_session_id.to_string(),
            node_of_agent: HashMap::new(),
            iter_counts: HashMap::new(),
            tournaments: HashMap::new(),
            child_controller: HashMap::new(),
            judge_matches: HashMap::new(),
        };
        run.refresh_scheduling();
        run.resolve_dependency_outcomes();
        Ok(run)
    }

    /// W0-ABI resume: rebuild an in-flight run by replaying which node agent-ids already completed
    /// (e.g. recovered from the session log after an interruption). Those nodes are pre-marked
    /// done so [`ready_batch`](Self::ready_batch) returns only the remaining work — the kernel then
    /// continues the DAG from where it left off.
    ///
    /// R3-1: `submissions` are the runtime [`Self::submit_nodes`] batches recorded (in order) before
    /// the interruption, re-applied **first** so dynamically-appended nodes reconstruct. When
    /// `submission_bases` records each batch's original base index (from the
    /// `WorkflowNodesSubmitted` observation), batches are re-applied at those EXACT indices,
    /// gap-filling any interleaved runtime children (tournament entrants/judges) with inert
    /// completed placeholders — so a later batch never shifts onto a child's old index and every
    /// terminal id maps to the node it originally named. Every submission requires its exact base.
    ///
    /// Loop iterations complete under `wf-node{N}-i{k}`: replay advances the iteration cursor to
    /// the highest recorded `k+1` instead of discarding the finished work — the node re-arms at the
    /// next iteration. It completes when `max_iters` is provably exhausted OR a recorded
    /// `loop_continue == false` proves the semantic early stop; otherwise the next iteration runs.
    ///
    /// A completed `Classify` node replays its recorded branch choice through the same prune logic
    /// as [`Self::record_completion`]; with no recorded choice every branch is pruned
    /// — the same "no recognizable choice" contract as the live path, and strictly safer than
    /// running a branch the original classification rejected.
    ///
    /// A completed `Tournament` controller is NOT replayed (its children are runtime appends the
    /// SDK never logs as node completions): a bracket unresolved at the interruption re-expands and
    /// re-runs from its entrants — wasteful but faithful.
    pub fn resume(
        spec: &WorkflowSpec,
        parent_session_id: &str,
        submissions: &[Vec<WorkflowNode>],
        submission_bases: &[u32],
        outcomes: &[ResumedNodeOutcome],
    ) -> Result<Self> {
        if submissions.len() != submission_bases.len() {
            return Err(DeepStrikeError::InvalidConfig(format!(
                "resume requires one base per submission ({} submissions, {} bases)",
                submissions.len(),
                submission_bases.len()
            )));
        }
        let mut run = Self::new(spec, parent_session_id)?;
        for (i, batch) in submissions.iter().enumerate() {
            if let Some(&base) = submission_bases.get(i) {
                let base = base as usize;
                if base < run.nodes.len() {
                    return Err(DeepStrikeError::InvalidConfig(format!(
                        "resume: submission {i} base {base} below reconstructed graph len {} — corrupt submission record",
                        run.nodes.len()
                    )));
                }
                // Gap = runtime children appended between batches (a restarting bracket).
                // Fill with inert completed placeholders so indices stay faithful; a child's
                // completed id then lands on its placeholder instead of a shifted later batch.
                while run.nodes.len() < base {
                    let idx = run.nodes.len();
                    let node = WorkflowNode::new(
                        RuntimeTask::new("[resume placeholder: runtime child slot]"),
                        AgentRole::Implement,
                    );
                    run.graph.add(node.task.clone(), Vec::new());
                    run.nodes.push(node);
                    run.graph.start(idx);
                    run.graph.complete(idx, resumed_placeholder_result());
                }
            }
            run.submit_nodes(batch.clone()).map_err(|error| {
                DeepStrikeError::InvalidConfig(format!("resume submission {i} rejected: {error}"))
            })?;
        }
        let n = run.graph.len();
        // Highest finished iteration + last recorded loop_continue per Loop node.
        let mut loop_cursor: HashMap<usize, (usize, Option<bool>)> = HashMap::new();
        for rec in outcomes {
            let id = rec.agent_id.as_str();
            if let Some(node) = (0..n).find(|&i| node_agent_id(i) == id) {
                run.graph.start(node);
                if let NodeKind::Classify { branches } = &run.nodes[node].kind {
                    // Re-prune exactly as record_completion: fail every branch the recorded
                    // choice did not select BEFORE completing, so promotion arms only the winner.
                    let chosen = rec.classify_branch.clone();
                    let prune: Vec<usize> = branches
                        .iter()
                        .filter(|b| Some(&b.label) != chosen.as_ref())
                        .flat_map(|b| b.nodes.iter().copied())
                        .collect();
                    for bn in prune {
                        run.graph.fail(bn);
                    }
                    let mut restored = rec.clone();
                    restored.classify_branch = chosen;
                    run.restore_outcome(node, &restored)?;
                } else {
                    run.restore_outcome(node, rec)?;
                }
                continue;
            }
            if let Some((node, k)) = parse_loop_iteration_id(id, n) {
                if matches!(run.nodes[node].kind, NodeKind::Loop { .. }) {
                    let entry = loop_cursor.entry(node).or_insert((0, None));
                    if k + 1 >= entry.0 {
                        entry.0 = k + 1;
                        // The HIGHEST iteration's signal decides (later records override).
                        entry.1 = rec.loop_continue;
                    }
                }
            }
        }
        for (node, (done, last_continue)) in loop_cursor {
            if let NodeKind::Loop { max_iters } = run.nodes[node].kind {
                let done = run.iter_counts.get(&node).copied().unwrap_or(0).max(done);
                run.iter_counts.insert(node, done);
                let stop_recorded = last_continue == Some(false);
                if done >= max_iters || stop_recorded {
                    run.graph.start(node);
                    let rec = outcomes
                        .iter()
                        .filter(|rec| {
                            parse_loop_iteration_id(&rec.agent_id, n)
                                .is_some_and(|(n, _)| n == node)
                        })
                        .max_by_key(|rec| {
                            parse_loop_iteration_id(&rec.agent_id, n)
                                .map(|(_, k)| k)
                                .unwrap_or(0)
                        })
                        .expect("loop cursor was built from at least one recovered outcome");
                    run.restore_outcome(node, rec)?;
                } else {
                    // Re-arm at the recorded cursor: the next spawn round runs
                    // `wf-node{node}-i{done}` instead of restarting from i0.
                    run.graph.set_ready(node);
                }
            }
        }
        run.resolve_dependency_outcomes();
        Ok(run)
    }

    fn restore_outcome(&mut self, node: usize, rec: &ResumedNodeOutcome) -> Result<()> {
        if rec.status == WorkflowNodeStatus::SkippedUpstreamFailed {
            if rec.termination.is_some() || rec.output.is_some() {
                return Err(DeepStrikeError::InvalidConfig(format!(
                    "resumed skipped node {} cannot carry termination/output",
                    rec.agent_id
                )));
            }
            self.graph.skip_upstream_failed(node);
            return Ok(());
        }
        let termination = rec.termination.ok_or_else(|| {
            DeepStrikeError::InvalidConfig(format!(
                "resumed node {} with status {:?} requires termination",
                rec.agent_id, rec.status
            ))
        })?;
        let expected = match termination {
            TerminationReason::Completed => WorkflowNodeStatus::Completed,
            TerminationReason::MaxTurns
            | TerminationReason::TokenBudget
            | TerminationReason::Timeout
            | TerminationReason::MilestoneExceeded
            | TerminationReason::ContextOverflow
            | TerminationReason::NoProgress => WorkflowNodeStatus::CompletedPartial,
            TerminationReason::Error | TerminationReason::UserAbort => WorkflowNodeStatus::Failed,
        };
        if rec.status != expected {
            return Err(DeepStrikeError::InvalidConfig(format!(
                "resumed node {} status {:?} conflicts with termination {:?}",
                rec.agent_id, rec.status, termination
            )));
        }
        let result = LoopResult {
            termination,
            final_message: rec.output.clone(),
            turns_used: 0,
            total_tokens_used: 0,
            loop_continue: rec.loop_continue,
            classify_branch: rec.classify_branch.clone(),
            tournament_winner: rec.tournament_winner.clone(),
            pace_decision: None,
        };
        match rec.status {
            WorkflowNodeStatus::Completed => self.graph.complete(node, result),
            WorkflowNodeStatus::CompletedPartial => self.graph.complete_partial(node, result),
            WorkflowNodeStatus::Failed => self.graph.fail_with_result(node, result),
            WorkflowNodeStatus::SkippedUpstreamFailed => unreachable!(),
        }
        Ok(())
    }

    /// Node indices whose dependencies are satisfied and that have not yet started.
    pub fn ready_batch(&mut self) -> Vec<usize> {
        self.graph.ready_tasks()
    }

    pub fn set_scheduler_policy(
        &mut self,
        policy: crate::scheduler::policy::SchedulerPolicyConfig,
    ) {
        self.scheduler_policy = policy;
        self.refresh_scheduling();
    }

    fn refresh_scheduling(&mut self) {
        let token_costs: Vec<u64> = self
            .nodes
            .iter()
            .map(|node| u64::from(node.token_budget.unwrap_or(0)))
            .collect();
        self.graph
            .configure_scheduling(self.scheduler_policy, &token_costs);
    }

    /// The agent id for a node's *current* spawn. For a `Spawn` node this is the stable
    /// `wf-node{N}`; for a `Loop` node it is `wf-node{N}-i{k}` where `k` is the count of iterations
    /// already finished — so each iteration gets a distinct id without any new ABI (the SDK simply
    /// spawns the id it is given and feeds it back as a `sub_agent_completed`).
    pub fn current_agent_id(&self, node: usize) -> String {
        match self.nodes[node].kind {
            NodeKind::Loop { .. } => {
                let k = self.iter_counts.get(&node).copied().unwrap_or(0);
                format!("{}-i{k}", node_agent_id(node))
            }
            // Spawn / Classify run once, a Tournament controller never spawns its own agent (its
            // entrant/judge children are separate Spawn nodes), and a Reduce node runs once as host
            // compute → stable plain id.
            NodeKind::Spawn
            | NodeKind::Classify { .. }
            | NodeKind::Tournament { .. }
            | NodeKind::Reduce { .. } => node_agent_id(node),
        }
    }

    /// Build the isolation manifest for a node's current spawn, preserving its explicit isolation +
    /// context-inheritance (the `AgentRunSpec`→`from_spec` path would overwrite these with
    /// role defaults). Capability inheritance for workflow nodes is left to a later round.
    pub fn manifest_for(&self, node: usize) -> IsolationManifest {
        let n = &self.nodes[node];
        IsolationManifest {
            agent_id: self.current_agent_id(node).into(),
            parent_session_id: self.parent_session_id.as_str().into(),
            role: n.role,
            isolation: n.isolation,
            context_inheritance: n.context_inheritance,
            permitted_capability_ids: Vec::new(),
        }
    }

    /// The goal text for a node (for the spawn's run spec / context injection).
    /// W3 quarantine invariant: a quarantined node reads untrusted content and must run read-only.
    /// Returns `true` if the node is `Quarantined` yet declares a write-capable isolation
    /// (`Shared`/`Worktree`/`Remote`) — a privilege contradiction the kernel refuses to spawn,
    /// turning the SDK's "self-discipline" quarantine into an in-kernel, auditable enforcement.
    pub fn quarantine_violation(&self, node: usize) -> bool {
        let n = &self.nodes[node];
        matches!(n.trust, NodeTrust::Quarantined)
            && !matches!(n.isolation, AgentIsolation::ReadOnly)
    }

    /// The SDK-facing spawn descriptor for a node (agent id + goal + canonical role/isolation/
    /// inheritance strings + model hint). The kernel owns the spec; this is how the goal reaches
    /// the host that runs the node.
    pub fn spawn_info(&self, node: usize) -> WorkflowSpawnInfo {
        let n = &self.nodes[node];
        // The stable agent ids of this node's dependencies. A Reduce node's registered function
        // consumes them (G2); EVERY other dependent node gets them too, so the SDK can put the
        // dependency outputs in the node's context — a DAG edge carries data, not just ordering
        // (without this, fan-out→synthesize produced an uninformed synthesis).
        let reducer = match &n.kind {
            NodeKind::Reduce { reducer } => Some(reducer.clone()),
            _ => None,
        };
        let input_agent_ids: Vec<String> = n.depends_on.iter().map(|&d| node_agent_id(d)).collect();
        // A#2 v2 / classify: surface the control-flow kind so the SDK can solicit + report the
        // matching result signal (`loop_continue` / `classify_branch`), mirroring how `reducer` /
        // `judge_match` distinguish reduce / judge spawns.
        let loop_max_iters = match &n.kind {
            NodeKind::Loop { max_iters } => Some(*max_iters),
            _ => None,
        };
        let classify_labels = match &n.kind {
            NodeKind::Classify { branches } => branches.iter().map(|b| b.label.clone()).collect(),
            _ => Vec::new(),
        };
        WorkflowSpawnInfo {
            agent_id: self.current_agent_id(node),
            goal: n.task.goal.clone(),
            role: role_label(n.role).to_string(),
            isolation: isolation_label(n.isolation).to_string(),
            context_inheritance: inheritance_label(n.context_inheritance).to_string(),
            model_hint: n.model_hint.clone(),
            trust: trust_label(n.trust).to_string(),
            output_schema: n.output_schema.clone(),
            reducer,
            input_agent_ids,
            judge_match: self.judge_matches.get(&node).cloned(),
            loop_max_iters,
            classify_labels,
            token_budget: n.token_budget,
            max_turns: n.max_turns,
            max_wall_ms: n.max_wall_ms,
        }
    }

    /// Mark a node as spawned: start it in the graph and map its kernel agent id back
    /// to the node for completion routing. (The live in-flight set is the executor's
    /// `SuspendState::SubAgentAwait.agent_ids` — the single source of in-flight truth.)
    pub fn mark_spawned(&mut self, node: usize, agent_id: &str) {
        self.graph.start(node);
        self.node_of_agent.insert(agent_id.to_string(), node);
    }

    /// Mark a node as denied by the syscall gate: fail it in the graph (dependents stay pending
    /// and will never become ready). Does not enter the live batch.
    pub fn mark_denied(&mut self, node: usize) {
        self.graph.fail(node);
        self.resolve_dependency_outcomes();
    }

    /// A host rejected or failed a spawn after the kernel reserved the node.
    /// Remove its completion route and fail the graph node so dependents cannot
    /// run on work that never started.
    pub fn mark_spawn_failed(&mut self, agent_id: &str) -> Option<usize> {
        let node = self.node_of_agent.remove(agent_id)?;
        self.graph.fail(node);
        self.resolve_dependency_outcomes();
        Some(node)
    }

    /// Record a completed sub-agent against its node. Returns the node index if `agent_id`
    /// belonged to this workflow, else `None`.
    ///
    /// For a `Loop` node this counts the finished iteration: while more iterations remain
    /// (`< max_iters`) the node is re-armed (`set_ready`) — so the next `ready_batch`/spawn round
    /// runs `wf-node{N}-i{k+1}` — and the node stays non-terminal, keeping its dependents pending.
    /// Only when the loop is exhausted is the node `complete`d, promoting its dependents.
    pub fn record_completion(&mut self, agent_id: &str, result: LoopResult) -> Option<usize> {
        let node = *self.node_of_agent.get(agent_id)?;

        // A tournament entrant/judge child: route the completion into its controller's bracket
        // rather than treating it as an ordinary node (it has no dependents of its own).
        if let Some(&controller) = self.child_controller.get(&node) {
            return self.advance_tournament(controller, node, result);
        }

        // A loop iteration with a non-success termination is itself terminal. Retrying it would
        // erase the partial/failure semantics behind a later successful iteration.
        if matches!(self.nodes[node].kind, NodeKind::Loop { .. })
            && result.termination != TerminationReason::Completed
        {
            self.settle_result(node, result);
            return Some(node);
        }

        match &self.nodes[node].kind {
            NodeKind::Loop { max_iters } => {
                // v2 semantic stop: the iteration may signal "done" (`loop_continue == Some(false)`),
                // ending the loop before `max_iters`. `None`/`Some(true)` run to the cap (v1 behavior).
                let max_iters = *max_iters;
                let stop_requested = result.loop_continue == Some(false);
                let done = self.iter_counts.entry(node).or_insert(0);
                *done += 1;
                if *done < max_iters && !stop_requested {
                    // More iterations: re-arm the node, keep it (and its dependents) in flight.
                    self.graph.set_ready(node);
                    return Some(node);
                }
            }
            NodeKind::Classify { branches } => {
                // Route to the branch matching the classifier's reported label; prune every other
                // branch's nodes (fail them) *before* completing this node, so that `complete`'s
                // dependent-promotion only arms the chosen branch (failed nodes are never re-armed).
                let chosen = result.classify_branch.clone();
                let prune: Vec<usize> = branches
                    .iter()
                    .filter(|b| Some(&b.label) != chosen.as_ref())
                    .flat_map(|b| b.nodes.iter().copied())
                    .collect();
                for bn in prune {
                    self.graph.fail(bn);
                }
            }
            // A Tournament controller never reaches here (it spawns no agent of its own; its
            // children route through `child_controller` above). A Reduce node completes like a Spawn
            // (its host-compute result feeds back as an ordinary completion). Defensive no-op.
            NodeKind::Spawn | NodeKind::Tournament { .. } | NodeKind::Reduce { .. } => {}
        }

        // Spawn node, loop's final iteration, or a completed classifier. Map the run termination
        // into an explicit node terminal state, then apply each dependent's declared policy.
        self.settle_result(node, result);
        Some(node)
    }

    fn settle_result(&mut self, node: usize, result: LoopResult) {
        match result.termination {
            TerminationReason::Completed => self.graph.complete(node, result),
            TerminationReason::MaxTurns
            | TerminationReason::TokenBudget
            | TerminationReason::Timeout
            | TerminationReason::MilestoneExceeded
            | TerminationReason::ContextOverflow
            | TerminationReason::NoProgress => self.graph.complete_partial(node, result),
            TerminationReason::Error | TerminationReason::UserAbort => {
                self.graph.fail_with_result(node, result)
            }
        }
        self.resolve_dependency_outcomes();
    }

    /// Resolve every pending node whose dependency state is now decisive. Skips cascade until the
    /// graph reaches a fixed point, so a failed ancestor cannot leave hidden pending descendants.
    fn resolve_dependency_outcomes(&mut self) {
        loop {
            let mut changed = false;
            for node in 0..self.nodes.len() {
                if self.graph.get(node).map(|n| n.status) != Some(TaskStatus::Pending) {
                    continue;
                }
                let policy = self.nodes[node].dep_policy;
                if policy == DependencyPolicy::Optional {
                    self.graph.set_ready(node);
                    changed = true;
                    continue;
                }
                let statuses: Vec<TaskStatus> = self.nodes[node]
                    .depends_on
                    .iter()
                    .filter_map(|&dep| self.graph.get(dep).map(|n| n.status))
                    .collect();
                let all_terminal = statuses.iter().all(|status| status.is_terminal());
                let impossible = match policy {
                    DependencyPolicy::AllSuccess => statuses.iter().any(|status| {
                        matches!(
                            status,
                            TaskStatus::CompletedPartial
                                | TaskStatus::Failed
                                | TaskStatus::SkippedUpstreamFailed
                        )
                    }),
                    DependencyPolicy::AcceptPartial => statuses.iter().any(|status| {
                        matches!(
                            status,
                            TaskStatus::Failed | TaskStatus::SkippedUpstreamFailed
                        )
                    }),
                    DependencyPolicy::AllTerminal | DependencyPolicy::Optional => false,
                };
                if impossible {
                    self.graph.skip_upstream_failed(node);
                    changed = true;
                } else if all_terminal {
                    self.graph.set_ready(node);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    // ── Tournament controller (A#2) ─────────────────────────────────────────────────────────────

    /// Append an entrant/judge *child* node (no dependencies → immediately Ready) and return its
    /// index. Keeps `self.nodes` and `self.graph` index-aligned (both grow in lockstep), so the
    /// child flows through the unchanged spawn loop as an ordinary `wf-node{idx}` spawn.
    fn append_child(&mut self, node: WorkflowNode) -> usize {
        let idx = self.graph.add(node.task.clone(), Vec::new());
        debug_assert_eq!(idx, self.nodes.len(), "graph/nodes index drift");
        self.nodes.push(node);
        self.refresh_scheduling();
        idx
    }

    /// Expand every tournament controller node whose dependencies are now satisfied (status
    /// `Ready`) into its entrant children. The controller is moved to `Running` (it spawns no agent
    /// of its own) and stays non-terminal until its bracket resolves. Called by the executor before
    /// each spawn round, so a controller behind upstream deps expands the moment those complete.
    pub fn expand_ready_controllers(&mut self) {
        let pending: Vec<usize> = (0..self.nodes.len())
            .filter(|i| !self.tournaments.contains_key(i))
            .filter(|&i| matches!(self.nodes[i].kind, NodeKind::Tournament { .. }))
            .filter(|&i| self.graph.get(i).map(|n| n.status) == Some(TaskStatus::Ready))
            .collect();
        for c in pending {
            self.expand_tournament(c);
        }
    }

    /// Fan a controller out into its entrant generators. Entrants run independent + read-only (a
    /// clean context per candidate, quarantine-safe), inheriting the controller's trust.
    fn expand_tournament(&mut self, c: usize) {
        let entrants = match &self.nodes[c].kind {
            NodeKind::Tournament { entrants } => entrants.clone(),
            _ => return,
        };
        let trust = self.nodes[c].trust;
        // Controller spawns no agent of its own → take it out of the ready set until we complete it.
        self.graph.start(c);
        // W-2: a runtime-submitted controller bypasses `WorkflowSpec::validate`, so the ≥2-entrant
        // invariant is re-checked here. A contest that cannot form fails the controller outright
        // (no champion) instead of stalling Running forever with no child ever reporting back.
        if entrants.len() < 2 {
            self.complete_tournament(c, None);
            return;
        }
        let mut entrant_nodes = Vec::with_capacity(entrants.len());
        for task in entrants {
            let child = WorkflowNode::new(task, AgentRole::Custom)
                .with_isolation(AgentIsolation::ReadOnly)
                .with_trust(trust);
            let idx = self.append_child(child);
            self.child_controller.insert(idx, c);
            entrant_nodes.push(idx);
        }
        let entrants_remaining = entrant_nodes.len();
        self.tournaments.insert(
            c,
            TournamentState {
                entrant_nodes,
                entrants_remaining,
                bracket: None,
                judge_nodes: Vec::new(),
                judge_winners: Vec::new(),
                judges_remaining: 0,
            },
        );
    }

    /// A tournament child (entrant or judge) completed: advance the controller's bracket. Returns
    /// the controller node index (the node that conceptually progressed).
    fn advance_tournament(
        &mut self,
        controller: usize,
        child: usize,
        result: LoopResult,
    ) -> Option<usize> {
        // The child has no dependents; mark it terminal so the graph's done/outcome accounting
        // works. An `Error`-terminated child is *failed* — the same contract as an ordinary node
        // (`record_completion`) — so outcome() reports it honestly. The bracket still advances:
        // an errored entrant simply fields an empty candidate (judges see it and prefer the other
        // side); an errored judge reports no winner, which surfaces as a no-champion bracket below.
        match result.termination {
            TerminationReason::Completed => self.graph.complete(child, result.clone()),
            TerminationReason::MaxTurns
            | TerminationReason::TokenBudget
            | TerminationReason::Timeout
            | TerminationReason::MilestoneExceeded
            | TerminationReason::ContextOverflow
            | TerminationReason::NoProgress => self.graph.complete_partial(child, result.clone()),
            TerminationReason::Error | TerminationReason::UserAbort => {
                self.graph.fail_with_result(child, result.clone())
            }
        }

        let in_entrant_phase = self.tournaments.get(&controller)?.bracket.is_none();
        if in_entrant_phase {
            let all_in = {
                let st = self.tournaments.get_mut(&controller)?;
                st.entrants_remaining = st.entrants_remaining.saturating_sub(1);
                st.entrants_remaining == 0
            };
            if all_in {
                self.begin_bracket(controller);
            }
        } else {
            let round_done = {
                let st = self.tournaments.get_mut(&controller)?;
                if let Some(pos) = st.judge_nodes.iter().position(|&n| n == child) {
                    st.judge_winners[pos] = result.tournament_winner.clone();
                }
                st.judges_remaining = st.judges_remaining.saturating_sub(1);
                st.judges_remaining == 0
            };
            if round_done {
                self.finish_round(controller);
            }
        }
        Some(controller)
    }

    /// All entrants are in: embed the bracket over their agent ids and emit round 1's judges.
    fn begin_bracket(&mut self, controller: usize) {
        let entrant_ids: Vec<EntrantId> = self
            .tournaments
            .get(&controller)
            .map(|st| st.entrant_nodes.iter().map(|&n| node_agent_id(n)).collect())
            .unwrap_or_default();
        // ≥2 entrants is guaranteed by `validate`; `Tournament::new` only rejects an empty field.
        let mut bracket = match Tournament::new(entrant_ids) {
            Ok(b) => b,
            Err(_) => return self.complete_tournament(controller, None),
        };
        let action = bracket.start();
        if let Some(st) = self.tournaments.get_mut(&controller) {
            st.bracket = Some(bracket);
        }
        self.apply_action(controller, action);
    }

    /// This round's judges all reported: feed the winners to the bracket and act on what comes next.
    fn finish_round(&mut self, controller: usize) {
        let winners: Vec<EntrantId> = self
            .tournaments
            .get(&controller)
            .map(|st| st.judge_winners.iter().filter_map(|w| w.clone()).collect())
            .unwrap_or_default();
        let action = {
            let st = match self.tournaments.get_mut(&controller) {
                Some(st) => st,
                None => return,
            };
            match st.bracket.as_mut() {
                // A judge that reported no winner shrinks `winners` below the match count, so
                // `feed_round` errors — we surface that as a tournament with no champion.
                Some(b) => b.feed_round(winners),
                None => return,
            }
        };
        match action {
            Ok(act) => self.apply_action(controller, act),
            Err(_) => self.complete_tournament(controller, None),
        }
    }

    /// Act on a bracket step: spawn the round's judges, or finish with the champion.
    fn apply_action(&mut self, controller: usize, action: TournamentAction) {
        match action {
            TournamentAction::JudgeRound { matches, .. } => self.emit_judges(controller, matches),
            TournamentAction::Done { winner, .. } => {
                self.complete_tournament(controller, Some(winner))
            }
        }
    }

    /// Append one judge child per match (bias-resistant `Verify`: read-only, no inherited context),
    /// each carrying its `JudgeMatch`. The controller's own goal is the judging criterion.
    fn emit_judges(&mut self, controller: usize, matches: Vec<Match>) {
        let criterion = self.nodes[controller].task.clone();
        let trust = self.nodes[controller].trust;
        let mut judge_nodes = Vec::with_capacity(matches.len());
        for m in &matches {
            let judge = WorkflowNode::new(criterion.clone(), AgentRole::Verify).with_trust(trust);
            let idx = self.append_child(judge);
            self.child_controller.insert(idx, controller);
            self.judge_matches.insert(
                idx,
                JudgeMatch {
                    left: m.left.clone(),
                    right: m.right.clone(),
                },
            );
            judge_nodes.push(idx);
        }
        if let Some(st) = self.tournaments.get_mut(&controller) {
            st.judge_winners = vec![None; judge_nodes.len()];
            st.judges_remaining = judge_nodes.len();
            st.judge_nodes = judge_nodes;
        }
    }

    /// Resolve the controller: drop its bracket state and `complete` it with the champion's id in
    /// `tournament_winner`, promoting its dependents. A bracket with NO champion (a judge reported
    /// no winner, or the bracket could not form) *fails* the controller instead — dependents that
    /// would consume `tournament_winner` must starve rather than run on a missing input, exactly
    /// like the dependents of an `Error`-terminated Spawn node.
    fn complete_tournament(&mut self, controller: usize, winner: Option<EntrantId>) {
        self.tournaments.remove(&controller);
        let Some(winner) = winner else {
            self.graph.fail(controller);
            self.resolve_dependency_outcomes();
            return;
        };
        let result = LoopResult {
            termination: TerminationReason::Completed,
            final_message: None,
            turns_used: 0,
            total_tokens_used: 0,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: Some(winner),
            pace_decision: None,
        };
        self.graph.complete(controller, result);
        self.resolve_dependency_outcomes();
    }

    // ── R3-1: runtime node submission (true loop-until-done / dynamic fan-out) ────────────────────

    /// Append a batch of nodes to the in-flight DAG at runtime — the kernel side of the dynamic
    /// "submit nodes" capability, generalizing the tournament's [`Self::append_child`]. A running
    /// node, on completion, can ask for more work to be spawned: unknown-size discovery
    /// (loop-until-done) and per-item fan-out (e.g. a claim-extractor spawning one verifier per
    /// claim) both reduce to "append these nodes now".
    ///
    /// Each submitted node's `depends_on` is batch-relative: both forward and backward edges are
    /// accepted when the complete batch is acyclic. Validation precedes mutation, so malformed
    /// references or control-flow nodes reject the entire batch atomically.
    ///
    /// Pure graph mutation: the caller (state machine) is responsible for routing the trigger
    /// through `evaluate_syscall` before calling this, keeping the kernel's zero-I/O contract.
    ///
    /// G1 no-privilege-escalation: when `submitter` names a [`NodeTrust::Quarantined`] node, every
    /// node in this submission is coerced to `Quarantined` before append. A quarantined agent read
    /// untrusted content (which may be adversarial), so the topology it asks for is itself untrusted:
    /// it must not be able to launch a *trusted* (or write-capable) child and thereby escape its
    /// sandbox. This is transitive taint — a quarantined origin's descendants inherit quarantine —
    /// the topological analogue of a process spawned by an untrusted process inheriting its label.
    /// Trusted (or absent) submitters pass through unchanged. The coercion is enforced here in the
    /// kernel rather than trusting the SDK, and composes with the spawn-time
    /// [`Self::quarantine_violation`] gate (a coerced node that also asked for write isolation is
    /// then denied at spawn).
    pub fn submit_nodes_from(
        &mut self,
        submitter: Option<&str>,
        mut nodes: Vec<WorkflowNode>,
    ) -> std::result::Result<Vec<usize>, WorkflowSubmissionError> {
        let submitter_quarantined = submitter.is_some_and(|s| self.is_agent_quarantined(s));
        if submitter_quarantined {
            for node in &mut nodes {
                node.trust = NodeTrust::Quarantined;
            }
        }
        self.submit_nodes(nodes)
    }

    pub fn submit_nodes(
        &mut self,
        mut nodes: Vec<WorkflowNode>,
    ) -> std::result::Result<Vec<usize>, WorkflowSubmissionError> {
        let base = self.nodes.len();
        let batch_len = nodes.len();
        for (node_index, node) in nodes.iter().enumerate() {
            if matches!(node.kind, NodeKind::Loop { max_iters: 0 }) {
                return Err(WorkflowSubmissionError {
                    node_index,
                    reason: "loop max_iters must be greater than zero".to_string(),
                });
            }
            if matches!(&node.kind, NodeKind::Tournament { entrants } if entrants.len() < 2) {
                return Err(WorkflowSubmissionError {
                    node_index,
                    reason: "tournament requires at least two entrants".to_string(),
                });
            }
            for &dependency in &node.depends_on {
                if dependency >= batch_len {
                    return Err(WorkflowSubmissionError {
                        node_index,
                        reason: format!(
                            "dependency {dependency} out of range for batch of {batch_len} nodes"
                        ),
                    });
                }
                if dependency == node_index {
                    return Err(WorkflowSubmissionError {
                        node_index,
                        reason: "node depends on itself".to_string(),
                    });
                }
            }
            if let NodeKind::Classify { branches } = &node.kind {
                for branch_node in branches.iter().flat_map(|br| br.nodes.iter().copied()) {
                    if branch_node >= batch_len {
                        return Err(WorkflowSubmissionError {
                            node_index,
                            reason: format!("classify branch node {branch_node} out of range"),
                        });
                    }
                    if branch_node == node_index {
                        return Err(WorkflowSubmissionError {
                            node_index,
                            reason: "classifier cannot select itself as a branch node".to_string(),
                        });
                    }
                    if !nodes[branch_node].depends_on.contains(&node_index) {
                        return Err(WorkflowSubmissionError {
                            node_index: branch_node,
                            reason: format!(
                                "classify branch node must depend on classifier {node_index}"
                            ),
                        });
                    }
                }
            }
        }

        if WorkflowSpec::new(nodes.clone()).validate().is_err() {
            return Err(WorkflowSubmissionError {
                node_index: 0,
                reason: "submission introduces a dependency cycle".to_string(),
            });
        }

        for node in &mut nodes {
            node.depends_on = node.depends_on.iter().map(|dep| base + dep).collect();
            if let NodeKind::Classify { branches } = &mut node.kind {
                for branch in branches {
                    branch.nodes = branch.nodes.iter().map(|node| base + node).collect();
                }
            }
        }

        let mut ids = Vec::with_capacity(nodes.len());
        for node in nodes {
            let deps = node.depends_on.clone();
            let idx = self.graph.add(node.task.clone(), deps);
            debug_assert_eq!(idx, self.nodes.len(), "graph/nodes index drift");
            self.nodes.push(node);
            ids.push(idx);
        }
        self.refresh_scheduling();
        self.resolve_dependency_outcomes();
        Ok(ids)
    }

    /// Whether `agent_id` belongs to this workflow.
    pub fn owns_agent(&self, agent_id: &str) -> bool {
        self.node_of_agent.contains_key(agent_id)
    }

    /// R3-3: whether the node behind `agent_id` is `Quarantined` (it read untrusted content). The
    /// kernel uses this to label that node's output as untrusted-origin when it crosses into the
    /// trusted parent context — the provenance half of the cross-boundary contract (shaping the
    /// output into a structured summary stays the SDK's job; the kernel cannot inspect content).
    pub fn is_agent_quarantined(&self, agent_id: &str) -> bool {
        self.node_of_agent
            .get(agent_id)
            .is_some_and(|&node| matches!(self.nodes[node].trust, NodeTrust::Quarantined))
    }

    /// Test instrument: true when no node is currently `Running` — the spawned batch has
    /// fully reported back. Derived from the graph; the executor's in-flight truth is
    /// `SuspendState::SubAgentAwait.agent_ids`.
    #[cfg(test)]
    pub(crate) fn batch_drained(&self) -> bool {
        !(0..self.graph.len()).any(|i| {
            matches!(
                self.graph.get(i).map(|n| &n.status),
                Some(crate::orchestration::task_graph::TaskStatus::Running)
            )
        })
    }

    /// Test instrument: every node reached one of the four terminal statuses.
    #[cfg(test)]
    pub(crate) fn is_complete(&self) -> bool {
        self.graph.all_done()
    }

    /// Close a workflow and return exactly one typed terminal outcome per graph node.
    pub fn finish(&mut self) -> Vec<WorkflowNodeOutcome> {
        for node in 0..self.graph.len() {
            match self.graph.get(node).map(|node| node.status) {
                Some(TaskStatus::Pending | TaskStatus::Ready) => {
                    self.graph.skip_upstream_failed(node)
                }
                Some(TaskStatus::Running) => self.graph.fail(node),
                _ => {}
            }
        }
        self.node_outcomes()
    }

    pub fn node_outcomes(&self) -> Vec<WorkflowNodeOutcome> {
        (0..self.graph.len())
            .filter_map(|node| {
                let graph_node = self.graph.get(node)?;
                let status = match graph_node.status {
                    TaskStatus::Completed => WorkflowNodeStatus::Completed,
                    TaskStatus::CompletedPartial => WorkflowNodeStatus::CompletedPartial,
                    TaskStatus::Failed => WorkflowNodeStatus::Failed,
                    TaskStatus::SkippedUpstreamFailed => WorkflowNodeStatus::SkippedUpstreamFailed,
                    TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Running => return None,
                };
                Some(WorkflowNodeOutcome {
                    node_id: node_agent_id(node),
                    status,
                    termination: graph_node.result.as_ref().map(|result| result.termination),
                    output: graph_node
                        .result
                        .as_ref()
                        .and_then(|result| result.final_message.clone()),
                })
            })
            .collect()
    }

    /// #2-B abort: produce the same typed terminal contract as ordinary completion. Running/ready
    /// nodes fail with `UserAbort`; nodes that never started are skipped behind the aborted graph.
    pub fn abort_outcomes(&self) -> Vec<WorkflowNodeOutcome> {
        (0..self.graph.len())
            .filter_map(|node| {
                let graph_node = self.graph.get(node)?;
                let (status, termination) = match graph_node.status {
                    TaskStatus::Completed => (
                        WorkflowNodeStatus::Completed,
                        graph_node.result.as_ref().map(|result| result.termination),
                    ),
                    TaskStatus::CompletedPartial => (
                        WorkflowNodeStatus::CompletedPartial,
                        graph_node.result.as_ref().map(|result| result.termination),
                    ),
                    TaskStatus::Pending | TaskStatus::SkippedUpstreamFailed => {
                        (WorkflowNodeStatus::SkippedUpstreamFailed, None)
                    }
                    TaskStatus::Ready | TaskStatus::Running | TaskStatus::Failed => (
                        WorkflowNodeStatus::Failed,
                        Some(
                            graph_node
                                .result
                                .as_ref()
                                .map_or(TerminationReason::UserAbort, |result| result.termination),
                        ),
                    ),
                };
                Some(WorkflowNodeOutcome {
                    node_id: node_agent_id(node),
                    status,
                    termination,
                    output: graph_node
                        .result
                        .as_ref()
                        .and_then(|result| result.final_message.clone()),
                })
            })
            .collect()
    }

    /// Total node count.
    pub fn len(&self) -> usize {
        self.graph.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::workflow::fanout_synthesize;
    use crate::types::result::{LoopResult, TerminationReason};
    use crate::types::task::RuntimeTask;

    fn done() -> LoopResult {
        LoopResult {
            termination: TerminationReason::Completed,
            final_message: None,
            turns_used: 1,
            total_tokens_used: 0,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: None,
            pace_decision: None,
        }
    }

    fn terminated(termination: TerminationReason) -> LoopResult {
        LoopResult {
            termination,
            ..done()
        }
    }

    fn fanout2() -> WorkflowRun {
        // 2 workers (nodes 0,1) → synthesize (node 2, depends on both)
        let spec = fanout_synthesize(
            vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
            RuntimeTask::new("synth"),
        );
        WorkflowRun::new(&spec, "parent-sess").unwrap()
    }

    /// A judge completion reporting its winning entrant id.
    fn judge_done(winner: &str) -> LoopResult {
        LoopResult {
            tournament_winner: Some(winner.to_string()),
            ..done()
        }
    }

    /// Mimic one executor spawn round on a `WorkflowRun`: expand any ready controllers, then mark
    /// every ready node spawned (mapping its current agent id). Returns the spawned `(node, id)`s.
    fn spawn_round(run: &mut WorkflowRun) -> Vec<(usize, String)> {
        run.expand_ready_controllers();
        let ready = run.ready_batch();
        let mut out = Vec::new();
        for node in ready {
            let id = run.current_agent_id(node);
            run.mark_spawned(node, &id);
            out.push((node, id));
        }
        out
    }

    fn outcome_ids(run: &WorkflowRun, status: WorkflowNodeStatus) -> Vec<String> {
        run.node_outcomes()
            .into_iter()
            .filter(|outcome| outcome.status == status)
            .map(|outcome| outcome.node_id)
            .collect()
    }

    #[test]
    fn first_batch_is_the_workers() {
        let mut run = fanout2();
        assert_eq!(run.ready_batch(), vec![0, 1]);
        assert_eq!(run.len(), 3);
        assert!(!run.is_complete());
    }

    // ── R3-1: runtime node submission ────────────────────────────────────────────────────────

    #[test]
    fn submit_nodes_appends_independent_nodes_ready_immediately() {
        use crate::orchestration::workflow::WorkflowNode;
        use crate::types::agent::AgentRole;

        let mut run = fanout2(); // nodes 0,1 (workers) → 2 (synth)
        assert_eq!(run.len(), 3);
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("extra-a"), AgentRole::Implement),
                WorkflowNode::new(RuntimeTask::new("extra-b"), AgentRole::Implement),
            ])
            .unwrap();
        assert_eq!(ids, vec![3, 4], "appended after the existing 3 nodes");
        assert_eq!(run.len(), 5);
        let ready = run.ready_batch();
        assert!(
            ready.contains(&3) && ready.contains(&4),
            "submitted independent nodes are immediately ready: {ready:?}"
        );
    }

    #[test]
    fn submitted_nodes_must_complete_before_workflow_is_done() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // A single spawn node that, on completion, submits more work (loop-until-done shape).
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());
        let ids = run
            .submit_nodes(vec![WorkflowNode::new(
                RuntimeTask::new("more"),
                AgentRole::Implement,
            )])
            .unwrap();
        assert_eq!(ids, vec![1]);
        assert!(
            !run.is_complete(),
            "not complete while the submitted node is pending"
        );
        let spawned = spawn_round(&mut run);
        assert_eq!(spawned, vec![(1usize, "wf-node1".to_string())]);
        run.record_completion("wf-node1", done());
        assert!(
            run.is_complete(),
            "complete once the submitted node finishes"
        );
    }

    #[test]
    fn reduce_node_carries_reducer_and_inputs_then_completes_like_a_spawn() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // G2: two fan-out workers feed a deterministic reduce node (dedupe). The reduce node runs no
        // agent; its descriptor names the reducer + its inputs, and it completes like a spawn.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("worker-a"), AgentRole::Explore),
            WorkflowNode::new(RuntimeTask::new("worker-b"), AgentRole::Explore),
            WorkflowNode::new(RuntimeTask::new("merge"), AgentRole::Implement)
                .with_reduce("dedupe_lines")
                .with_depends_on(vec![0, 1]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();

        // Only the two workers are ready first (the reduce node waits on both).
        assert_eq!(run.ready_batch(), vec![0, 1]);
        for i in [0usize, 1] {
            let id = run.current_agent_id(i);
            run.mark_spawned(i, &id);
            run.record_completion(&id, done());
        }

        // Now the reduce node is ready; its descriptor carries the reducer name + both input ids.
        assert_eq!(run.ready_batch(), vec![2]);
        let info = run.spawn_info(2);
        assert_eq!(info.reducer.as_deref(), Some("dedupe_lines"));
        assert_eq!(
            info.input_agent_ids,
            vec!["wf-node0".to_string(), "wf-node1".to_string()]
        );

        // The reduce node's (SDK-computed) result feeds back as an ordinary completion → DAG done.
        run.mark_spawned(2, "wf-node2");
        run.record_completion("wf-node2", done());
        assert!(run.is_complete());
        let completed = outcome_ids(&run, WorkflowNodeStatus::Completed);
        assert_eq!(completed, vec!["wf-node0", "wf-node1", "wf-node2"]);
        assert_eq!(run.node_outcomes().len(), completed.len());
    }

    #[test]
    fn output_schema_reaches_the_spawn_descriptor() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // G3: a node declaring an output schema carries it verbatim to the SDK spawn descriptor.
        let schema = serde_json::json!({
            "type": "object",
            "required": ["verdict"],
            "properties": { "verdict": { "type": "string" } }
        });
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("judge"), AgentRole::Verify)
                .with_output_schema(schema.clone()),
        ]);
        let run = WorkflowRun::new(&spec, "sess").unwrap();
        let info = run.spawn_info(0);
        assert_eq!(info.output_schema.as_ref(), Some(&schema));

        // Full serde round-trip preserves it (additive ABI).
        let json = serde_json::to_string(&info).unwrap();
        let back: WorkflowSpawnInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.output_schema, Some(schema));

        // A node without a schema omits the field entirely on the wire.
        let plain = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("x"),
            AgentRole::Implement,
        )]);
        let plain_info = WorkflowRun::new(&plain, "sess").unwrap().spawn_info(0);
        assert!(plain_info.output_schema.is_none());
        assert!(
            !serde_json::to_string(&plain_info)
                .unwrap()
                .contains("output_schema")
        );
    }

    #[test]
    fn quarantined_submitter_taints_submitted_nodes() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // G1: a quarantined root reads untrusted content, then tries to submit a node it declares
        // "trusted" (and write-capable). The kernel must coerce that node to quarantined — a
        // quarantined origin cannot escalate its descendants out of the sandbox.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("read-untrusted"), AgentRole::Explore).quarantined(),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());

        // Submitted node claims Trusted; the quarantined submitter cannot grant that.
        let ids = run
            .submit_nodes_from(
                Some(&id0),
                vec![WorkflowNode::new(
                    RuntimeTask::new("act"),
                    AgentRole::Implement,
                )],
            )
            .unwrap();
        assert_eq!(ids, vec![1]);
        let id1 = run.current_agent_id(1);
        run.mark_spawned(1, &id1);
        assert!(
            run.is_agent_quarantined(&id1),
            "submitted node inherits the submitter's quarantine (no escalation)"
        );

        // A trusted / unknown submitter does NOT coerce — only quarantined origins taint.
        let ids2 = run
            .submit_nodes_from(
                None,
                vec![WorkflowNode::new(
                    RuntimeTask::new("trusted-work"),
                    AgentRole::Implement,
                )],
            )
            .unwrap();
        let id2 = run.current_agent_id(ids2[0]);
        run.mark_spawned(ids2[0], &id2);
        assert!(
            !run.is_agent_quarantined(&id2),
            "no quarantined submitter ⇒ no coercion"
        );
    }

    #[test]
    fn submit_nodes_honors_batch_relative_backward_deps() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());
        // [extractor @offset 0, dependent @offset 1 depends on 0].
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("extractor"), AgentRole::Implement),
                WorkflowNode::new(RuntimeTask::new("dependent"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ])
            .unwrap();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(
            run.ready_batch(),
            vec![1],
            "backward dep keeps the dependent pending"
        );
        run.mark_spawned(1, "wf-node1");
        run.record_completion("wf-node1", done());
        assert_eq!(
            run.ready_batch(),
            vec![2],
            "dependent unblocks after the extractor"
        );
    }

    #[test]
    fn submit_nodes_accepts_acyclic_forward_dependencies() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        // Node 0 in the batch depends on node 1: a valid forward edge. Only node 1 starts.
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("consumer"), AgentRole::Implement)
                    .with_depends_on(vec![1]),
                WorkflowNode::new(RuntimeTask::new("producer"), AgentRole::Implement),
            ])
            .unwrap();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(
            run.ready_batch(),
            vec![2, 0],
            "the producer is on the longer critical path and runs before the independent root"
        );
        run.mark_spawned(2, "wf-node2");
        run.record_completion("wf-node2", done());
        assert!(run.ready_batch().contains(&1));
    }

    #[test]
    fn submit_nodes_rejects_malformed_batches_atomically() {
        use crate::orchestration::workflow::WorkflowNode;
        use crate::types::agent::AgentRole;

        for nodes in [
            vec![
                WorkflowNode::new(RuntimeTask::new("bad-range"), AgentRole::Implement)
                    .with_depends_on(vec![1]),
            ],
            vec![
                WorkflowNode::new(RuntimeTask::new("self"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ],
            vec![
                WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement)
                    .with_depends_on(vec![1]),
                WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ],
        ] {
            let mut run = fanout2();
            let before = run.len();
            assert!(run.submit_nodes(nodes).is_err());
            assert_eq!(run.len(), before, "rejection must precede every mutation");
        }
    }

    #[test]
    fn submitted_node_can_itself_be_a_loop_control_flow() {
        // R3-2: control flow *composes* through dynamic submission — a submitted node can itself be
        // a Loop (or Tournament), executing its full control flow. This delivers nested control flow
        // without changing `NodeKind::Tournament`'s entrant type: the submitter just hands over a
        // node whose `kind` the unchanged completion machinery already honors.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());

        // Submit a Loop{2} node mid-run.
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("refine"), AgentRole::Implement).with_loop(2),
            ])
            .unwrap();
        assert_eq!(ids, vec![1]);

        // It iterates with distinct per-iteration ids, then completes — its control flow runs.
        for k in 0..2 {
            assert_eq!(
                run.ready_batch(),
                vec![1],
                "submitted loop ready for iteration {k}"
            );
            let id = run.current_agent_id(1);
            assert_eq!(
                id,
                format!("wf-node1-i{k}"),
                "submitted loop gets per-iteration ids"
            );
            run.mark_spawned(1, &id);
            run.record_completion(&id, done());
        }
        assert!(
            run.is_complete(),
            "submitted loop ran its 2 iterations then finished"
        );
    }

    #[test]
    fn submitted_tournament_runs_bracket_then_promotes_submitted_dependent() {
        // M2: an agent can submit a Tournament *controller* (plus a dependent) at runtime. The
        // controller expands into entrant children + a judge via the same bracket machinery, and the
        // dependent's batch-relative `depends_on` links it to the submitted controller.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());

        // Submit [tournament@batch0, dependent@batch1 depends_on [0]] (batch-relative).
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("pick best"), AgentRole::Plan)
                    .with_tournament(vec![RuntimeTask::new("x"), RuntimeTask::new("y")]),
                WorkflowNode::new(RuntimeTask::new("use winner"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ])
            .unwrap();
        assert_eq!(ids, vec![1, 2], "appended controller=1, dependent=2");

        // Controller (node 1) expands into 2 entrant children (3,4); spawns no agent of its own.
        let entrants = spawn_round(&mut run);
        let entrant_nodes: Vec<usize> = entrants.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            entrant_nodes,
            vec![3, 4],
            "two entrant children appended after the dependent"
        );
        for (_, id) in &entrants {
            run.record_completion(id, done());
        }

        // One judge over the two entrants; dependent (node 2) gated until the bracket resolves.
        let r1 = spawn_round(&mut run);
        assert_eq!(r1.len(), 1, "one judge for two entrants");
        let jm = run
            .spawn_info(r1[0].0)
            .judge_match
            .expect("judge carries a match");
        assert_eq!(
            jm,
            JudgeMatch {
                left: node_agent_id(3),
                right: node_agent_id(4)
            }
        );

        // Entrant 3 wins → controller completes with the champion → dependent unblocks.
        run.record_completion(&r1[0].1, judge_done(&node_agent_id(3)));
        assert_eq!(
            run.ready_batch(),
            vec![2],
            "submitted dependent unblocks after the bracket"
        );
        let last = spawn_round(&mut run);
        assert_eq!(last, vec![(2, node_agent_id(2))]);
        run.record_completion(&last[0].1, done());
        assert!(run.is_complete());
    }

    #[test]
    fn submitted_classify_remaps_branch_indices_and_prunes() {
        // M2: a submitted Classify node's branch `nodes` are batch-relative; `submit_nodes` remaps
        // them to absolute indices so the chosen branch runs and the rest are pruned. Without the
        // remap a runtime-submitted classifier would prune the wrong nodes.
        use crate::orchestration::workflow::{
            ClassifyBranch, NodeKind, WorkflowNode, WorkflowSpec,
        };
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());

        // Submit [classify@batch0 (a→[1] b→[2]), branchA@batch1 dep[0], branchB@batch2 dep[0]].
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("route"), AgentRole::Plan).with_classify(vec![
                    ClassifyBranch {
                        label: "a".into(),
                        nodes: vec![1],
                    },
                    ClassifyBranch {
                        label: "b".into(),
                        nodes: vec![2],
                    },
                ]),
                WorkflowNode::new(RuntimeTask::new("branch-a"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
                WorkflowNode::new(RuntimeTask::new("branch-b"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ])
            .unwrap();
        assert_eq!(ids, vec![1, 2, 3], "classify=1, branchA=2, branchB=3");

        // Branch indices were remapped batch-relative → absolute: a→[2], b→[3].
        if let NodeKind::Classify { branches } = &run.nodes[1].kind {
            assert_eq!(
                branches[0].nodes,
                vec![2],
                "branch a remapped to absolute node 2"
            );
            assert_eq!(
                branches[1].nodes,
                vec![3],
                "branch b remapped to absolute node 3"
            );
        } else {
            panic!("node 1 should be a classify node");
        }

        // Classifier picks "a" → branch-a (node 2) runs, branch-b (node 3) is pruned/failed.
        let r = spawn_round(&mut run);
        assert_eq!(r, vec![(1, node_agent_id(1))], "classifier runs first");
        run.record_completion(
            &r[0].1,
            LoopResult {
                classify_branch: Some("a".into()),
                ..done()
            },
        );

        assert_eq!(run.ready_batch(), vec![2], "only branch a is enabled");
        let failed = outcome_ids(&run, WorkflowNodeStatus::Failed);
        assert!(
            failed.contains(&node_agent_id(3)),
            "branch b is explicitly failed by routing"
        );

        let last = spawn_round(&mut run);
        assert_eq!(last, vec![(2, node_agent_id(2))]);
        run.record_completion(&last[0].1, done());
        assert!(run.is_complete());
        let completed = outcome_ids(&run, WorkflowNodeStatus::Completed);
        assert!(completed.contains(&node_agent_id(1)) && completed.contains(&node_agent_id(2)));
    }

    #[test]
    fn loop_node_iterates_with_distinct_ids_then_promotes_dependent() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // node 0 = Loop{3}; node 1 depends on node 0 (must wait for the whole loop).
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("refine"), AgentRole::Implement).with_loop(3),
            WorkflowNode::new(RuntimeTask::new("finalize"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();

        // Three iterations, each with a distinct agent id; the dependent stays unready throughout.
        for k in 0..3 {
            assert_eq!(
                run.ready_batch(),
                vec![0],
                "loop node ready for iteration {k}"
            );
            let id = run.current_agent_id(0);
            assert_eq!(id, format!("wf-node0-i{k}"), "distinct per-iteration id");
            run.mark_spawned(0, &id);
            assert!(!run.is_complete());
            let node = run.record_completion(&id, done()).unwrap();
            assert_eq!(node, 0);
            if k < 2 {
                // Loop continues: node 0 re-armed, dependent NOT yet ready.
                assert_eq!(run.ready_batch(), vec![0]);
            }
        }

        // Loop exhausted → node 0 complete → dependent (node 1) becomes ready.
        assert_eq!(
            run.ready_batch(),
            vec![1],
            "dependent unblocks only after the loop ends"
        );
        let id1 = run.current_agent_id(1);
        assert_eq!(id1, "wf-node1", "spawn node keeps the plain id");
        run.mark_spawned(1, &id1);
        run.record_completion(&id1, done());
        assert!(run.is_complete());
    }

    #[test]
    fn synth_becomes_ready_only_after_both_workers() {
        let mut run = fanout2();
        for &n in &[0usize, 1usize] {
            let id = node_agent_id(n);
            run.mark_spawned(n, &id);
        }
        assert!(!run.batch_drained());
        // first worker completes → synth not ready yet, batch not drained
        assert_eq!(run.record_completion(&node_agent_id(0), done()), Some(0));
        assert!(!run.batch_drained());
        assert!(run.ready_batch().is_empty());
        // second worker completes → batch drained, synth now ready
        assert_eq!(run.record_completion(&node_agent_id(1), done()), Some(1));
        assert!(run.batch_drained());
        assert_eq!(run.ready_batch(), vec![2]);
        assert!(!run.is_complete());
        // spawn + complete synth → workflow complete
        run.mark_spawned(2, &node_agent_id(2));
        run.record_completion(&node_agent_id(2), done());
        assert!(run.is_complete());
    }

    #[test]
    fn denied_node_skips_dependents_and_closes_outcome() {
        let mut run = fanout2();
        // node 0 spawned + completes; node 1 denied by the gate
        run.mark_spawned(0, &node_agent_id(0));
        run.mark_denied(1);
        run.record_completion(&node_agent_id(0), done());
        assert!(run.batch_drained());
        assert!(run.ready_batch().is_empty());
        assert!(run.is_complete());
        let outcomes = run.finish();
        assert_eq!(outcomes.len(), 3);
        assert_eq!(outcomes[0].status, WorkflowNodeStatus::Completed);
        assert_eq!(outcomes[1].status, WorkflowNodeStatus::Failed);
        assert_eq!(
            outcomes[2].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );
    }

    #[test]
    fn terminal_mapping_and_dependency_policies_are_explicit() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let cases = [
            (TerminationReason::Completed, WorkflowNodeStatus::Completed),
            (
                TerminationReason::MaxTurns,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::TokenBudget,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::Timeout,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::ContextOverflow,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::NoProgress,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::MilestoneExceeded,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (TerminationReason::Error, WorkflowNodeStatus::Failed),
            (TerminationReason::UserAbort, WorkflowNodeStatus::Failed),
        ];
        for (termination, expected) in cases {
            let spec = WorkflowSpec::new(vec![WorkflowNode::new(
                RuntimeTask::new("node"),
                AgentRole::Implement,
            )]);
            let mut run = WorkflowRun::new(&spec, "sess").unwrap();
            run.mark_spawned(0, "wf-node0");
            run.record_completion("wf-node0", terminated(termination));
            let outcome = run.finish().remove(0);
            assert_eq!(outcome.status, expected);
            assert_eq!(outcome.termination, Some(termination));
        }

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("upstream"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("strict"), AgentRole::Implement)
                .with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("partial-ok"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::AcceptPartial),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Timeout));
        assert_eq!(run.ready_batch(), vec![2]);
        assert_eq!(
            run.node_outcomes()[1].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );
    }

    #[test]
    fn all_terminal_and_optional_have_distinct_waiting_semantics() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("upstream"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("cleanup"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::AllTerminal),
            WorkflowNode::new(RuntimeTask::new("best-effort"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::Optional),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        assert_eq!(run.ready_batch(), vec![0, 2]);
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Error));
        assert!(run.ready_batch().contains(&1));
    }

    #[test]
    fn loop_terminal_result_is_not_retried() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("loop"), AgentRole::Implement).with_loop(3),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        run.mark_spawned(0, "wf-node0-i0");
        run.record_completion("wf-node0-i0", terminated(TerminationReason::Timeout));
        assert!(run.ready_batch().is_empty());
        assert_eq!(run.finish()[0].status, WorkflowNodeStatus::CompletedPartial);
    }

    #[test]
    fn manifest_preserves_node_isolation_and_inheritance() {
        let run = fanout2();
        let m = run.manifest_for(0);
        assert_eq!(m.agent_id.as_str(), "wf-node0");
        assert_eq!(m.parent_session_id.as_str(), "parent-sess");
        // fanout workers are Explore → ReadOnly + SystemOnly (workflow role_defaults)
        assert_eq!(m.isolation, crate::types::agent::AgentIsolation::ReadOnly);
        assert_eq!(
            m.context_inheritance,
            crate::types::agent::ContextInheritance::SystemOnly
        );
    }

    #[test]
    fn unknown_agent_completion_is_none() {
        let mut run = fanout2();
        assert_eq!(run.record_completion("not-a-node", done()), None);
    }

    #[test]
    fn resume_skips_already_completed_nodes() {
        // fanout2: workers 0,1 → synth 2. Resume with worker 0 already done.
        let spec = fanout_synthesize(
            vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
            RuntimeTask::new("synth"),
        );
        let mut run = WorkflowRun::resume(
            &spec,
            "sess",
            &[],
            &[],
            &[ResumedNodeOutcome::completed(node_agent_id(0))],
        )
        .unwrap();
        // only the remaining worker (node 1) is ready; node 0 is already complete, synth still gated.
        assert_eq!(run.ready_batch(), vec![1]);
        assert!(!run.is_complete());
    }

    #[test]
    fn resume_with_all_done_completes() {
        let spec = fanout_synthesize(vec![RuntimeTask::new("w0")], RuntimeTask::new("synth"));
        // both nodes (worker 0, synth 1) recovered as done.
        let mut run = WorkflowRun::resume(
            &spec,
            "sess",
            &[],
            &[],
            &[
                ResumedNodeOutcome::completed(node_agent_id(0)),
                ResumedNodeOutcome::completed(node_agent_id(1)),
            ],
        )
        .unwrap();
        assert!(run.ready_batch().is_empty());
        assert!(run.is_complete());
    }

    #[test]
    fn resume_preserves_partial_and_failed_dependency_semantics() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("upstream"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("strict"), AgentRole::Implement)
                .with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("partial-ok"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::AcceptPartial),
        ]);

        let partial = ResumedNodeOutcome {
            agent_id: node_agent_id(0),
            status: WorkflowNodeStatus::CompletedPartial,
            termination: Some(TerminationReason::Timeout),
            output: None,
            classify_branch: None,
            tournament_winner: None,
            loop_continue: None,
        };
        let mut resumed = WorkflowRun::resume(&spec, "sess", &[], &[], &[partial]).unwrap();
        assert_eq!(resumed.ready_batch(), vec![2]);
        assert_eq!(
            resumed.node_outcomes()[0].status,
            WorkflowNodeStatus::CompletedPartial
        );
        assert_eq!(
            resumed.node_outcomes()[1].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );

        let failed = ResumedNodeOutcome {
            agent_id: node_agent_id(0),
            status: WorkflowNodeStatus::Failed,
            termination: Some(TerminationReason::Error),
            output: None,
            classify_branch: None,
            tournament_winner: None,
            loop_continue: None,
        };
        let mut resumed = WorkflowRun::resume(&spec, "sess", &[], &[], &[failed]).unwrap();
        assert!(resumed.ready_batch().is_empty());
        assert_eq!(
            resumed.node_outcomes()[0].status,
            WorkflowNodeStatus::Failed
        );
        assert_eq!(
            resumed.node_outcomes()[1].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );
        assert_eq!(
            resumed.node_outcomes()[2].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );
    }

    #[test]
    fn resume_applies_submissions_at_recorded_bases_with_placeholder_gap_fill() {
        // The interleave bug: original run = spec [node0] → tournament children at 1,2 →
        // runtime submission at base 3. A missing base would otherwise replay it at index 1 and
        // the child's completed id "wf-node2" would mark the WRONG node. With the recorded
        // base, indices stay faithful: 1,2 become inert completed placeholders, the batch
        // lands at 3, and every completed id maps to the node it originally named.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let submission = vec![WorkflowNode::new(
            RuntimeTask::new("late batch"),
            AgentRole::Implement,
        )];
        let mut run = WorkflowRun::resume(
            &spec,
            "sess",
            &[submission],
            &[3],
            &[
                ResumedNodeOutcome::completed("wf-node2"),
                ResumedNodeOutcome::completed("wf-node3"),
            ],
        )
        .unwrap();
        assert_eq!(
            run.graph.len(),
            4,
            "spec node + 2 placeholders + 1 submitted"
        );
        // The submitted node (3) is complete because ITS id was recorded — not shifted.
        assert!(
            run.ready_batch() == vec![0],
            "only the spec node remains to run"
        );
        // A base below the reconstructed length is corrupt, not silently reinterpreted.
        let spec2 = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement),
        ]);
        let bad = WorkflowRun::resume(
            &spec2,
            "sess",
            &[vec![WorkflowNode::new(
                RuntimeTask::new("x"),
                AgentRole::Implement,
            )]],
            &[1],
            &[],
        );
        assert!(
            bad.is_err(),
            "base inside the spec range is a corrupt record"
        );
        let missing_base = WorkflowRun::resume(
            &spec2,
            "sess",
            &[vec![WorkflowNode::new(
                RuntimeTask::new("x"),
                AgentRole::Implement,
            )]],
            &[],
            &[],
        );
        assert!(
            missing_base.is_err(),
            "every recovered submission requires an exact base"
        );
    }

    #[test]
    fn resume_restores_loop_iteration_cursor_instead_of_restarting() {
        // A 3-iteration loop with iterations 0 and 1 already finished pre-interruption:
        // resume must re-arm the node at i2 (not silently restart from i0).
        use crate::orchestration::workflow::{NodeKind, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut node =
            WorkflowNode::new(RuntimeTask::new("polish until done"), AgentRole::Implement);
        node.kind = NodeKind::Loop { max_iters: 3 };
        let spec = WorkflowSpec::new(vec![node]);
        let mut run = WorkflowRun::resume(
            &spec,
            "sess",
            &[],
            &[],
            &[
                ResumedNodeOutcome::completed("wf-node0-i0"),
                ResumedNodeOutcome::completed("wf-node0-i1"),
            ],
        )
        .unwrap();
        assert_eq!(
            run.ready_batch(),
            vec![0],
            "loop node re-armed, not complete"
        );
        assert_eq!(
            run.current_agent_id(0),
            "wf-node0-i2",
            "cursor advanced past finished work"
        );
        assert!(!run.is_complete());
    }

    #[test]
    fn resume_completes_loop_when_all_iterations_recorded() {
        use crate::orchestration::workflow::{NodeKind, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut node = WorkflowNode::new(RuntimeTask::new("polish"), AgentRole::Implement);
        node.kind = NodeKind::Loop { max_iters: 2 };
        let spec = WorkflowSpec::new(vec![node]);
        let mut run = WorkflowRun::resume(
            &spec,
            "sess",
            &[],
            &[],
            &[
                ResumedNodeOutcome::completed("wf-node0-i0"),
                ResumedNodeOutcome::completed("wf-node0-i1"),
            ],
        )
        .unwrap();
        assert!(run.ready_batch().is_empty());
        assert!(
            run.is_complete(),
            "max_iters provably exhausted -> node complete"
        );
    }

    #[test]
    fn resume_reapplies_submissions_to_reconstruct_appended_nodes() {
        // R3-1: a workflow that dynamically appended a node (wf-node1) is resumed by re-applying the
        // recorded submission, so the appended node exists again and its completed id matches —
        // without this, the appended node (not in the spec) would vanish on resume.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let submission = vec![WorkflowNode::new(
            RuntimeTask::new("discovered"),
            AgentRole::Implement,
        )];

        // root done, submission re-applied, but the appended node not yet completed.
        let mut run = WorkflowRun::resume(
            &spec,
            "sess",
            &[submission.clone()],
            &[1],
            &[ResumedNodeOutcome::completed(node_agent_id(0))],
        )
        .unwrap();
        assert_eq!(run.len(), 2, "base node + re-applied submitted node");
        assert_eq!(
            run.ready_batch(),
            vec![1],
            "the re-applied appended node is the remaining work"
        );
        assert!(!run.is_complete());

        // both recovered as done → resume finishes.
        let mut run2 = WorkflowRun::resume(
            &spec,
            "sess",
            &[submission],
            &[1],
            &[
                ResumedNodeOutcome::completed(node_agent_id(0)),
                ResumedNodeOutcome::completed(node_agent_id(1)),
            ],
        )
        .unwrap();
        assert!(run2.ready_batch().is_empty());
        assert!(run2.is_complete());
    }

    #[test]
    fn spawn_info_carries_model_hint_and_trust() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("read tickets"), AgentRole::Explore)
                .quarantined()
                .with_model_hint("haiku"),
            WorkflowNode::new(RuntimeTask::new("act"), AgentRole::Implement),
        ]);
        let run = WorkflowRun::new(&spec, "sess").unwrap();

        // W3: quarantined node + W4: model hint both reach the spawn descriptor.
        let q = run.spawn_info(0);
        assert_eq!(q.trust, "quarantined");
        assert_eq!(q.model_hint.as_deref(), Some("haiku"));
        // default node is trusted, no model hint.
        let t = run.spawn_info(1);
        assert_eq!(t.trust, "trusted");
        assert_eq!(t.model_hint, None);
    }

    #[test]
    fn spawn_info_carries_loop_and_classify_hints() {
        use crate::orchestration::workflow::{ClassifyBranch, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            // 0: loop node → descriptor carries the cap so the SDK knows to solicit `loop_continue`.
            WorkflowNode::new(RuntimeTask::new("refine"), AgentRole::Implement).with_loop(3),
            // 1: classify node → descriptor carries the branch labels so the SDK can instruct + report.
            WorkflowNode::new(RuntimeTask::new("route"), AgentRole::Plan).with_classify(vec![
                ClassifyBranch {
                    label: "bug".into(),
                    nodes: vec![],
                },
                ClassifyBranch {
                    label: "feature".into(),
                    nodes: vec![],
                },
            ]),
            // 2: plain spawn → neither hint present.
            WorkflowNode::new(RuntimeTask::new("act"), AgentRole::Implement),
        ]);
        let run = WorkflowRun::new(&spec, "sess").unwrap();

        let l = run.spawn_info(0);
        assert_eq!(l.loop_max_iters, Some(3));
        assert!(l.classify_labels.is_empty());
        assert_eq!(l.token_budget, None, "no token budget unless set");

        let c = run.spawn_info(1);
        assert_eq!(
            c.classify_labels,
            vec!["bug".to_string(), "feature".to_string()]
        );
        assert_eq!(c.loop_max_iters, None);

        let s = run.spawn_info(2);
        assert_eq!(s.loop_max_iters, None);
        assert!(s.classify_labels.is_empty());
    }

    #[test]
    fn spawn_info_carries_token_budget() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("expensive"), AgentRole::Implement)
                .with_token_budget(10_000),
            WorkflowNode::new(RuntimeTask::new("plain"), AgentRole::Implement),
        ]);
        let run = WorkflowRun::new(&spec, "sess").unwrap();
        assert_eq!(run.spawn_info(0).token_budget, Some(10_000));
        assert_eq!(run.spawn_info(1).token_budget, None);
    }

    // ── Tournament node (A#2) ───────────────────────────────────────────────────────────────────

    use crate::orchestration::workflow::{NodeKind, WorkflowNode, WorkflowSpec};
    use crate::types::agent::AgentRole;

    /// A 4-entrant tournament controller (node 0) gating a dependent (node 1). Drives the whole
    /// bracket: 4 entrants generate, then 2 round-1 judges, then 1 final judge — and only then does
    /// the dependent unblock, carrying the champion in the controller's `tournament_winner`.
    #[test]
    fn tournament_runs_bracket_then_promotes_dependent() {
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("pick the best ad"), AgentRole::Plan)
                .with_tournament(vec![
                    RuntimeTask::new("ad A"),
                    RuntimeTask::new("ad B"),
                    RuntimeTask::new("ad C"),
                    RuntimeTask::new("ad D"),
                ]),
            WorkflowNode::new(RuntimeTask::new("ship the winner"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();

        // Round 1 of spawning expands the controller into 4 entrant children (nodes 2..=5); the
        // controller spawns no agent of its own and the dependent stays gated.
        let entrants = spawn_round(&mut run);
        let entrant_nodes: Vec<usize> = entrants.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            entrant_nodes,
            vec![2, 3, 4, 5],
            "4 entrant children, no controller spawn"
        );
        assert!(
            run.spawn_info(2).judge_match.is_none(),
            "entrants are not judges"
        );
        assert!(!run.is_complete());

        // All entrants generate → bracket begins; nothing else spawns until they're all in.
        for (i, (node, id)) in entrants.iter().enumerate() {
            run.record_completion(id, done());
            if i < 3 {
                assert!(
                    run.ready_batch().is_empty(),
                    "no judges until every entrant is in"
                );
            }
            let _ = node;
        }

        // Round 1 judges: 2 matches over the 4 entrants, each carrying its pair.
        let r1 = spawn_round(&mut run);
        assert_eq!(r1.len(), 2, "two round-1 judges");
        let jm0 = run
            .spawn_info(r1[0].0)
            .judge_match
            .expect("judge carries a match");
        assert_eq!(
            jm0,
            JudgeMatch {
                left: node_agent_id(2),
                right: node_agent_id(3)
            }
        );
        let jm1 = run
            .spawn_info(r1[1].0)
            .judge_match
            .expect("judge carries a match");
        assert_eq!(
            jm1,
            JudgeMatch {
                left: node_agent_id(4),
                right: node_agent_id(5)
            }
        );

        // Entrant 2 beats 3; entrant 4 beats 5. Dependent still gated mid-bracket.
        run.record_completion(&r1[0].1, judge_done(&node_agent_id(2)));
        run.record_completion(&r1[1].1, judge_done(&node_agent_id(4)));
        assert!(
            run.ready_batch().iter().all(|&n| n != 1),
            "dependent gated until the final"
        );

        // Final round: a single judge over the two survivors.
        let r2 = spawn_round(&mut run);
        assert_eq!(r2.len(), 1, "one final judge");
        let jmf = run
            .spawn_info(r2[0].0)
            .judge_match
            .expect("final judge carries a match");
        assert_eq!(
            jmf,
            JudgeMatch {
                left: node_agent_id(2),
                right: node_agent_id(4)
            }
        );

        // Entrant 4 wins it all → controller completes with the champion, dependent unblocks.
        run.record_completion(&r2[0].1, judge_done(&node_agent_id(4)));
        let winner = run
            .graph
            .get(0)
            .and_then(|n| n.result.as_ref())
            .and_then(|r| r.tournament_winner.clone());
        assert_eq!(
            winner.as_deref(),
            Some(node_agent_id(4).as_str()),
            "champion recorded"
        );
        assert_eq!(
            run.ready_batch(),
            vec![1],
            "dependent unblocks only after the bracket resolves"
        );

        // Ship the winner → workflow complete.
        let last = spawn_round(&mut run);
        assert_eq!(last, vec![(1, node_agent_id(1))]);
        run.record_completion(&last[0].1, done());
        assert!(run.is_complete());
    }

    /// An odd entrant count gives one entrant a bye in round 1 (no judge for it), and the bracket
    /// still resolves to a single champion.
    #[test]
    fn tournament_with_bye_resolves() {
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("rank"), AgentRole::Plan).with_tournament(vec![
                RuntimeTask::new("x"),
                RuntimeTask::new("y"),
                RuntimeTask::new("z"),
            ]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();

        let entrants = spawn_round(&mut run); // nodes 1,2,3
        assert_eq!(entrants.len(), 3);
        for (_, id) in &entrants {
            run.record_completion(id, done());
        }
        // Round 1: only (entrant1, entrant2) plays; entrant3 draws a bye.
        let r1 = spawn_round(&mut run);
        assert_eq!(r1.len(), 1, "one match, one bye");
        run.record_completion(&r1[0].1, judge_done(&node_agent_id(1)));
        // Round 2: survivor of the match vs the bye entrant.
        let r2 = spawn_round(&mut run);
        assert_eq!(r2.len(), 1);
        let jm = run.spawn_info(r2[0].0).judge_match.unwrap();
        assert_eq!(
            jm,
            JudgeMatch {
                left: node_agent_id(1),
                right: node_agent_id(3)
            }
        );
        run.record_completion(&r2[0].1, judge_done(&node_agent_id(3)));
        let winner = run
            .graph
            .get(0)
            .and_then(|n| n.result.as_ref())
            .and_then(|r| r.tournament_winner.clone());
        assert_eq!(winner.as_deref(), Some(node_agent_id(3).as_str()));
        assert!(run.is_complete());
    }

    /// A quarantined tournament keeps its entrant + judge children quarantined, and (being
    /// read-only) they pass the quarantine invariant rather than tripping it.
    #[test]
    fn tournament_children_inherit_controller_trust() {
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("judge untrusted inputs"), AgentRole::Plan)
                .quarantined()
                .with_tournament(vec![RuntimeTask::new("a"), RuntimeTask::new("b")]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();

        let entrants = spawn_round(&mut run);
        for (node, _) in &entrants {
            assert_eq!(
                run.spawn_info(*node).trust,
                "quarantined",
                "entrant inherits quarantine"
            );
            assert!(
                !run.quarantine_violation(*node),
                "read-only entrant is quarantine-clean"
            );
        }
        for (_, id) in &entrants {
            run.record_completion(id, done());
        }
        let r1 = spawn_round(&mut run);
        assert_eq!(
            run.spawn_info(r1[0].0).trust,
            "quarantined",
            "judge inherits quarantine"
        );
        assert!(!run.quarantine_violation(r1[0].0));
    }

    /// Sanity: the controller node is itself a Tournament kind and never appears in a spawn batch
    /// (entrants/judges carry the work).
    #[test]
    fn tournament_controller_never_spawns_itself() {
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("c"), AgentRole::Plan)
                .with_tournament(vec![RuntimeTask::new("a"), RuntimeTask::new("b")]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        assert!(matches!(run.nodes[0].kind, NodeKind::Tournament { .. }));
        let first = spawn_round(&mut run);
        assert!(
            first.iter().all(|(n, _)| *n != 0),
            "controller node 0 never spawns directly"
        );
    }

    // ── dynamic-workflow optimization batch (W-1..W-6, W-N2/N7) ─────────────────────────────────

    use crate::orchestration::workflow::ClassifyBranch;

    fn classify_spec() -> WorkflowSpec {
        // node0 classifies into branch "a" → node1, branch "b" → node2.
        let classifier = WorkflowNode::new(RuntimeTask::new("route"), AgentRole::Plan)
            .with_classify(vec![
                ClassifyBranch {
                    label: "a".to_string(),
                    nodes: vec![1],
                },
                ClassifyBranch {
                    label: "b".to_string(),
                    nodes: vec![2],
                },
            ]);
        WorkflowSpec::new(vec![
            classifier,
            WorkflowNode::new(RuntimeTask::new("on a"), AgentRole::Implement)
                .with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("on b"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ])
    }

    #[test]
    fn resume_replays_classify_prune_from_recorded_branch() {
        // W-1: pre-crash the classifier chose "a" (node2 was pruned). Resume must re-prune node2
        // from the recorded signal — without it the REJECTED branch would run after resume.
        let mut run = WorkflowRun::resume(
            &classify_spec(),
            "sess",
            &[],
            &[],
            &[ResumedNodeOutcome {
                agent_id: "wf-node0".to_string(),
                classify_branch: Some("a".to_string()),
                ..ResumedNodeOutcome::completed("unused")
            }],
        )
        .unwrap();
        assert_eq!(
            run.ready_batch(),
            vec![1],
            "only the chosen branch is armed"
        );
        let failed = outcome_ids(&run, WorkflowNodeStatus::Failed);
        assert_eq!(
            failed,
            vec!["wf-node2"],
            "rejected branch stays pruned across resume"
        );
    }

    #[test]
    fn resume_with_signalless_classify_record_prunes_all_branches() {
        // Legacy log (bare id, no recorded branch): prune every branch — the live path's
        // "no recognizable choice" contract, and strictly safer than running a rejected branch.
        let mut run = WorkflowRun::resume(
            &classify_spec(),
            "sess",
            &[],
            &[],
            &[ResumedNodeOutcome::completed("wf-node0")],
        )
        .unwrap();
        assert!(run.ready_batch().is_empty());
        let failed = outcome_ids(&run, WorkflowNodeStatus::Failed);
        assert_eq!(failed, vec!["wf-node1", "wf-node2"]);
        assert!(run.is_complete());
    }

    #[test]
    fn resume_honors_recorded_loop_stop() {
        // W-1: iteration 0 recorded `loop_continue=false` — the semantic stop is now provable from
        // the log, so the node completes instead of re-running the final iteration.
        let mut node = WorkflowNode::new(RuntimeTask::new("polish"), AgentRole::Implement);
        node.kind = NodeKind::Loop { max_iters: 3 };
        let spec = WorkflowSpec::new(vec![node]);
        let mut run = WorkflowRun::resume(
            &spec,
            "sess",
            &[],
            &[],
            &[ResumedNodeOutcome {
                agent_id: "wf-node0-i0".to_string(),
                loop_continue: Some(false),
                ..ResumedNodeOutcome::completed("unused")
            }],
        )
        .unwrap();
        assert!(
            run.ready_batch().is_empty(),
            "no re-run of the stopped loop"
        );
        assert!(run.is_complete());
    }

    #[test]
    fn errored_tournament_child_is_failed_and_no_champion_fails_controller() {
        // W-3: an Error-terminated entrant is FAILED (same contract as record_completion), and a
        // bracket with no champion FAILS the controller so dependents starve on the missing winner.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("pick"), AgentRole::Plan)
                .with_tournament(vec![RuntimeTask::new("x"), RuntimeTask::new("y")]),
            WorkflowNode::new(RuntimeTask::new("use winner"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        let entrants = spawn_round(&mut run);
        assert_eq!(entrants.len(), 2);
        run.record_completion(&entrants[0].1, done());
        run.record_completion(
            &entrants[1].1,
            LoopResult {
                termination: TerminationReason::Error,
                ..done()
            },
        );
        // Bracket forms; the single judge reports NO winner (e.g. it errored too).
        let judges = spawn_round(&mut run);
        assert_eq!(judges.len(), 1, "one match for two entrants");
        run.record_completion(&judges[0].1, done()); // tournament_winner: None
        let failed = outcome_ids(&run, WorkflowNodeStatus::Failed);
        assert!(
            failed.contains(&entrants[1].1),
            "errored entrant reported failed"
        );
        assert!(
            failed.contains(&"wf-node0".to_string()),
            "no-champion controller failed"
        );
        assert!(
            !run.ready_batch().contains(&1),
            "dependent of the failed controller starves"
        );
    }

    #[test]
    fn submitted_tournament_with_one_entrant_is_rejected_atomically() {
        let mut run = fanout2();
        let before = run.len();
        let controller = WorkflowNode::new(RuntimeTask::new("pick"), AgentRole::Plan)
            .with_tournament(vec![RuntimeTask::new("only")]);
        assert!(run.submit_nodes(vec![controller]).is_err());
        assert_eq!(run.len(), before);
    }

    #[test]
    fn submitted_classify_branch_without_classifier_dependency_is_rejected() {
        let mut run = fanout2();
        let before = run.len();
        let classifier = WorkflowNode::new(RuntimeTask::new("route"), AgentRole::Plan)
            .with_classify(vec![ClassifyBranch {
                label: "a".to_string(),
                nodes: vec![1],
            }]);
        let branch = WorkflowNode::new(RuntimeTask::new("on a"), AgentRole::Implement);
        assert!(run.submit_nodes(vec![classifier, branch]).is_err());
        assert_eq!(run.len(), before);
    }

    #[test]
    fn submitted_zero_iter_loop_is_rejected() {
        let mut run = fanout2();
        let before = run.len();
        let mut node = WorkflowNode::new(RuntimeTask::new("once"), AgentRole::Implement);
        node.kind = NodeKind::Loop { max_iters: 0 };
        assert!(run.submit_nodes(vec![node]).is_err());
        assert_eq!(run.len(), before);
    }

    #[test]
    fn spawn_info_carries_dep_ids_and_per_node_caps() {
        // W-N2: EVERY dependent node carries its dependencies' agent ids (a DAG edge carries data);
        // W-N7: per-node max_turns/max_wall_ms ride the same hop chain as token_budget.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("w"), AgentRole::Explore),
            WorkflowNode::new(RuntimeTask::new("synth"), AgentRole::Plan)
                .with_depends_on(vec![0])
                .with_max_turns(4)
                .with_max_wall_ms(30_000),
        ]);
        let run = WorkflowRun::new(&spec, "sess").unwrap();
        let info = run.spawn_info(1);
        assert_eq!(info.input_agent_ids, vec!["wf-node0"]);
        assert_eq!(info.max_turns, Some(4));
        assert_eq!(info.max_wall_ms, Some(30_000));
        assert!(info.reducer.is_none(), "plain node stays non-reduce");
        let root = run.spawn_info(0);
        assert!(root.input_agent_ids.is_empty());
        assert_eq!(root.max_turns, None);
    }

    // ── P3 DAG scheduler scenarios (F1 critical-path / F2 loop fairness / F3 failure propagation) ─
    //
    // These make the deferred orchestration A/B concrete: with `scheduler_policy` now a first-class
    // config axis, scheduling behavior is single-variable testable instead of agent-driven. Each
    // scenario drives the deterministic scheduler and asserts the property the audit called out.

    /// F1 — critical-path skew: among ready siblings, the one heading the longest downstream chain is
    /// scheduled first, overriding node-id order.
    #[test]
    fn f1_critical_path_node_is_scheduled_before_a_lower_id_leaf() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::scheduler::policy::SchedulerPolicyConfig;
        use crate::types::agent::AgentRole;

        // node 0 = a leaf (critical path 1). node 1 = root of chain 1→2→3 (critical path 3).
        // Both are ready at the start; the longer critical path must win over the smaller id.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("leaf"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("chain-root"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("mid"), AgentRole::Implement).with_depends_on(vec![1]),
            WorkflowNode::new(RuntimeTask::new("tail"), AgentRole::Implement).with_depends_on(vec![2]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        run.set_scheduler_policy(SchedulerPolicyConfig::default());

        assert_eq!(
            run.ready_batch(),
            vec![1, 0],
            "the deeper critical path (node 1) outranks the lower-id leaf (node 0)"
        );
    }

    /// F2 — loop fairness: a re-arming loop node does not starve an independent ready node. The
    /// audit's starvation case (concurrency 1, loop with a smaller id) must let the independent node
    /// run between iterations.
    #[test]
    fn f2_rearming_loop_does_not_starve_an_independent_node() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::scheduler::policy::SchedulerPolicyConfig;
        use crate::types::agent::AgentRole;

        // node 0 = Loop{5} (smaller id), node 1 = independent plain node. Nothing depends on either.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("loop"), AgentRole::Implement).with_loop(5),
            WorkflowNode::new(RuntimeTask::new("independent"), AgentRole::Implement),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        run.set_scheduler_policy(SchedulerPolicyConfig::default());

        // Concurrency 1: run only the head of each ready batch. First round the loop wins the tie
        // (enqueued first); after it re-arms, the independent node — waiting since round 0 — must be
        // scheduled before the loop's next iteration.
        let first = run.ready_batch();
        assert_eq!(first[0], 0, "loop takes the first slot on the initial tie");
        let id = run.current_agent_id(0);
        run.mark_spawned(0, &id);
        run.record_completion(&id, done()); // finishes iteration 0, re-arms the loop

        assert_eq!(
            run.ready_batch()[0],
            1,
            "the independent node runs before the loop's second iteration (no starvation)"
        );
    }

    /// F3 — failure propagation: a failure skips its transitive successors, and partial results gate
    /// per the downstream node's dependency policy.
    #[test]
    fn f3_failure_and_partial_propagate_transitively_by_policy() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // Transitive skip: A(0) fails → B(1) depends on A → C(2) depends on B. Both B and C must
        // close as SkippedUpstreamFailed, not strand in Pending.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement).with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("c"), AgentRole::Implement).with_depends_on(vec![1]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Error));
        let outcomes = run.finish();
        assert_eq!(outcomes[0].status, WorkflowNodeStatus::Failed);
        assert_eq!(outcomes[1].status, WorkflowNodeStatus::SkippedUpstreamFailed);
        assert_eq!(
            outcomes[2].status,
            WorkflowNodeStatus::SkippedUpstreamFailed,
            "the failure propagates through the whole chain"
        );

        // Partial gates by policy: an upstream partial blocks an AllSuccess dependent but satisfies
        // an AcceptPartial one.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("up"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("strict"), AgentRole::Implement)
                .with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("lenient"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::AcceptPartial),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Timeout)); // partial
        assert_eq!(run.ready_batch(), vec![2], "only the AcceptPartial dependent runs");
        assert_eq!(
            run.node_outcomes()[1].status,
            WorkflowNodeStatus::SkippedUpstreamFailed,
            "the AllSuccess dependent is skipped behind the partial upstream"
        );
    }

    // ── P3 (P0-4) generative property: every node lands in exactly one terminal set ─────────────
    //
    // The audit flagged that totality ("∀ node ∈ exactly one terminal state") held structurally but
    // had no generative coverage — the old executor could strand blocked successors in Pending,
    // appearing in neither the completed nor failed audit set. This drives many pseudo-random DAGs
    // with random dependency policies to completion under random per-node terminations (including
    // denials) and asserts `finish()` closes every node into exactly one terminal status. A
    // regression means the workflow audit is no longer closed.

    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 ^ (self.0 >> 33)) % n.max(1)
        }
    }

    #[test]
    fn finish_closes_every_node_into_exactly_one_terminal_state_over_random_dags() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;
        use std::collections::BTreeSet;

        let terminations = [
            TerminationReason::Completed,
            TerminationReason::MaxTurns,
            TerminationReason::TokenBudget,
            TerminationReason::Timeout,
            TerminationReason::ContextOverflow,
            TerminationReason::NoProgress,
            TerminationReason::MilestoneExceeded,
            TerminationReason::Error,
            TerminationReason::UserAbort,
        ];
        let policies = [
            DependencyPolicy::AllSuccess,
            DependencyPolicy::AcceptPartial,
            DependencyPolicy::AllTerminal,
            DependencyPolicy::Optional,
        ];

        for seed in 0..300u64 {
            let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1));
            let n = 2 + rng.below(7) as usize;

            // Random DAG: node i depends on a random subset of earlier nodes (backward edges only,
            // so it is always acyclic), with a random dependency policy.
            let mut nodes = Vec::new();
            for i in 0..n {
                let mut deps = Vec::new();
                for j in 0..i {
                    if rng.below(3) == 0 {
                        deps.push(j);
                    }
                }
                let policy = policies[rng.below(policies.len() as u64) as usize];
                nodes.push(
                    WorkflowNode::new(RuntimeTask::new(format!("n{i}")), AgentRole::Implement)
                        .with_depends_on(deps)
                        .with_dependency_policy(policy),
                );
            }
            let spec = WorkflowSpec::new(nodes);
            let mut run = WorkflowRun::new(&spec, "sess").unwrap();

            // Drive to a fixpoint: spawn each ready node, then either complete it with a random
            // termination or deny it. Bounded so a bug cannot hang the test.
            for _ in 0..(n * 4 + 4) {
                let ready = run.ready_batch();
                if ready.is_empty() {
                    break;
                }
                for node in ready {
                    let agent = node_agent_id(node);
                    run.mark_spawned(node, &agent);
                    if rng.below(5) == 0 {
                        run.mark_denied(node);
                    } else {
                        let termination = terminations[rng.below(terminations.len() as u64) as usize];
                        run.record_completion(&agent, terminated(termination));
                    }
                }
            }

            let outcomes = run.finish();
            // Totality: exactly one terminal outcome per node, covering the whole node set.
            assert_eq!(outcomes.len(), n, "seed {seed}: every node has an outcome");
            let ids: BTreeSet<String> = outcomes.iter().map(|o| o.node_id.clone()).collect();
            assert_eq!(ids.len(), n, "seed {seed}: node ids are unique");
            for node in 0..n {
                assert!(
                    ids.contains(&node_agent_id(node)),
                    "seed {seed}: node {node} is in the closed outcome set"
                );
            }
            for outcome in &outcomes {
                assert!(
                    matches!(
                        outcome.status,
                        WorkflowNodeStatus::Completed
                            | WorkflowNodeStatus::CompletedPartial
                            | WorkflowNodeStatus::Failed
                            | WorkflowNodeStatus::SkippedUpstreamFailed
                    ),
                    "seed {seed}: {} is terminal",
                    outcome.node_id
                );
            }
        }
    }
}
