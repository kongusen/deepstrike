//! W0: a kernel-resident workflow run — the DAG state for one in-flight [`WorkflowSpec`].
//!
//! Pure data + pure advance logic, no I/O and no syscall: the [`crate::scheduler::state_machine::
//! LoopStateMachine`] drives this, gating each ready node's spawn through
//! `evaluate_syscall(Syscall::Spawn)` and reusing the existing batch-await barrier
//! (`SuspendState::SubAgentAwait`). This module only tracks *which* nodes are ready, spawned,
//! done, or denied, and builds each node's [`IsolationManifest`].
//!
//! Lifecycle: `ready_batch()` → (gate each) `mark_spawned` / `mark_denied` → on completion
//! `record_completion` → repeat until `is_complete()`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::orchestration::executor;
use crate::orchestration::task_graph::{TaskGraph, TaskStatus};
use crate::orchestration::tournament::{EntrantId, Match, Tournament, TournamentAction};
use crate::orchestration::workflow::{NodeKind, NodeTrust, WorkflowNode, WorkflowSpec};
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
use crate::types::error::Result;
use crate::types::result::{LoopResult, TerminationReason};

/// Deterministic kernel agent id for a workflow node (stable across resume / audit).
pub fn node_agent_id(node: usize) -> String {
    format!("wf-node{node}")
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
    /// Present only for a tournament *judge* spawn (A#2): the two entrant agent ids whose outputs
    /// this judge must compare. The SDK looks up those entrants' produced candidates, runs the
    /// judge, and reports the winner in the result's `tournament_winner`. `None` for every ordinary
    /// (entrant / spawn / loop / classify) node. Additive ABI: omitted on the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_match: Option<JudgeMatch>,
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

/// Synthetic terminal result for a node recovered as already-completed during resume.
fn resumed_result() -> LoopResult {
    LoopResult {
        termination: crate::types::result::TerminationReason::Completed,
        final_message: None,
        turns_used: 0,
        total_tokens_used: 0,
        loop_continue: None,
        classify_branch: None,
        tournament_winner: None,
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
    /// Parent session id stamped onto each node's spawned-agent manifest.
    parent_session_id: String,
    /// Completed-event lookup: kernel agent id → DAG node index.
    node_of_agent: HashMap<String, usize>,
    /// Nodes spawned in the current batch, awaiting completion.
    batch: Vec<usize>,
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
        spec.validate()?;
        Ok(Self {
            graph: spec.to_task_graph()?,
            nodes: spec.nodes.clone(),
            parent_session_id: parent_session_id.to_string(),
            node_of_agent: HashMap::new(),
            batch: Vec::new(),
            iter_counts: HashMap::new(),
            tournaments: HashMap::new(),
            child_controller: HashMap::new(),
            judge_matches: HashMap::new(),
        })
    }

    /// W0-ABI resume: rebuild an in-flight run by replaying which node agent-ids already completed
    /// (e.g. recovered from the session log after an interruption). Those nodes are pre-marked
    /// done so [`ready_batch`](Self::ready_batch) returns only the remaining work — the kernel then
    /// continues the DAG from where it left off. Unknown ids are ignored.
    pub fn resume(spec: &WorkflowSpec, parent_session_id: &str, completed: &[String]) -> Result<Self> {
        let mut run = Self::new(spec, parent_session_id)?;
        let n = run.graph.len();
        for id in completed {
            if let Some(node) = (0..n).find(|&i| node_agent_id(i) == *id) {
                run.graph.start(node);
                run.graph.complete(node, resumed_result());
            }
        }
        Ok(run)
    }

    /// Node indices whose dependencies are satisfied and that have not yet started.
    pub fn ready_batch(&self) -> Vec<usize> {
        executor::next_batch(&self.graph).runnable
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
            // Spawn / Classify run once, and a Tournament controller never spawns its own agent
            // (its entrant/judge children are separate Spawn nodes) → stable plain id.
            NodeKind::Spawn | NodeKind::Classify { .. } | NodeKind::Tournament { .. } => {
                node_agent_id(node)
            }
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
    pub fn goal_of(&self, node: usize) -> &str {
        &self.nodes[node].task.goal
    }

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
        WorkflowSpawnInfo {
            agent_id: self.current_agent_id(node),
            goal: n.task.goal.clone(),
            role: role_label(n.role).to_string(),
            isolation: isolation_label(n.isolation).to_string(),
            context_inheritance: inheritance_label(n.context_inheritance).to_string(),
            model_hint: n.model_hint.clone(),
            trust: trust_label(n.trust).to_string(),
            judge_match: self.judge_matches.get(&node).cloned(),
        }
    }

    /// Mark a node as spawned: start it in the graph, record it in the live batch, and map its
    /// kernel agent id back to the node for completion routing.
    pub fn mark_spawned(&mut self, node: usize, agent_id: &str) {
        self.graph.start(node);
        self.batch.push(node);
        self.node_of_agent.insert(agent_id.to_string(), node);
    }

    /// Mark a node as denied by the syscall gate: fail it in the graph (dependents stay pending
    /// and will never become ready). Does not enter the live batch.
    pub fn mark_denied(&mut self, node: usize) {
        self.graph.fail(node);
    }

    /// Record a completed sub-agent against its node. Returns the node index if `agent_id`
    /// belonged to this workflow (and removes it from the live batch), else `None`.
    ///
    /// For a `Loop` node this counts the finished iteration: while more iterations remain
    /// (`< max_iters`) the node is re-armed (`set_ready`) — so the next `ready_batch`/spawn round
    /// runs `wf-node{N}-i{k+1}` — and the node stays non-terminal, keeping its dependents pending.
    /// Only when the loop is exhausted is the node `complete`d, promoting its dependents.
    pub fn record_completion(&mut self, agent_id: &str, result: LoopResult) -> Option<usize> {
        let node = *self.node_of_agent.get(agent_id)?;
        self.batch.retain(|&n| n != node);

        // A tournament entrant/judge child: route the completion into its controller's bracket
        // rather than treating it as an ordinary node (it has no dependents of its own).
        if let Some(&controller) = self.child_controller.get(&node) {
            return self.advance_tournament(controller, node, result);
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
            // children route through `child_controller` above). Defensive no-op for completeness.
            NodeKind::Spawn | NodeKind::Tournament { .. } => {}
        }

        // Spawn node, loop's final iteration, or a completed classifier: promote dependents.
        self.graph.complete(node, result);
        Some(node)
    }

    // ── Tournament controller (A#2) ─────────────────────────────────────────────────────────────

    /// Append an entrant/judge *child* node (no dependencies → immediately Ready) and return its
    /// index. Keeps `self.nodes` and `self.graph` index-aligned (both grow in lockstep), so the
    /// child flows through the unchanged spawn loop as an ordinary `wf-node{idx}` spawn.
    fn append_child(&mut self, node: WorkflowNode) -> usize {
        let idx = self.graph.add(node.task.clone(), Vec::new());
        debug_assert_eq!(idx, self.nodes.len(), "graph/nodes index drift");
        self.nodes.push(node);
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
        // The child has no dependents; mark it terminal so the graph's done/outcome accounting works.
        self.graph.complete(child, result.clone());

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
    /// `tournament_winner`, promoting its dependents.
    fn complete_tournament(&mut self, controller: usize, winner: Option<EntrantId>) {
        self.tournaments.remove(&controller);
        let result = LoopResult {
            termination: TerminationReason::Completed,
            final_message: None,
            turns_used: 0,
            total_tokens_used: 0,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: winner,
        };
        self.graph.complete(controller, result);
    }

    /// Whether `agent_id` belongs to this workflow.
    pub fn owns_agent(&self, agent_id: &str) -> bool {
        self.node_of_agent.contains_key(agent_id)
    }

    /// The parent session id for this workflow (stamped on each node's manifest).
    pub fn parent_session_id(&self) -> &str {
        &self.parent_session_id
    }

    /// True once the current batch has drained (every spawned node reported back).
    pub fn batch_drained(&self) -> bool {
        self.batch.is_empty()
    }

    /// True once every node is terminal (completed or failed) and nothing is in flight.
    pub fn is_complete(&self) -> bool {
        self.graph.all_done() && self.batch.is_empty()
    }

    /// Outcome at finish: `(completed_agent_ids, failed_agent_ids)` by node. Nodes left
    /// `Pending`/`Ready` (stalled behind a gated dependency) appear in neither.
    pub fn outcome(&self) -> (Vec<String>, Vec<String>) {
        let mut completed = Vec::new();
        let mut failed = Vec::new();
        for i in 0..self.graph.len() {
            match self.graph.get(i).map(|n| n.status) {
                Some(TaskStatus::Completed) => completed.push(node_agent_id(i)),
                Some(TaskStatus::Failed) => failed.push(node_agent_id(i)),
                _ => {}
            }
        }
        (completed, failed)
    }

    /// Total node count.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
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

    #[test]
    fn first_batch_is_the_workers() {
        let run = fanout2();
        assert_eq!(run.ready_batch(), vec![0, 1]);
        assert_eq!(run.len(), 3);
        assert!(!run.is_complete());
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
            assert_eq!(run.ready_batch(), vec![0], "loop node ready for iteration {k}");
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
        assert_eq!(run.ready_batch(), vec![1], "dependent unblocks only after the loop ends");
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
    fn denied_node_blocks_dependents_and_stalls_progress() {
        let mut run = fanout2();
        // node 0 spawned + completes; node 1 denied by the gate
        run.mark_spawned(0, &node_agent_id(0));
        run.mark_denied(1);
        run.record_completion(&node_agent_id(0), done());
        // synth depends on node 1 (failed) → never ready; batch drained, nothing more to run.
        // The state machine finishes a workflow on "drained && ready_batch empty" (here true),
        // even though `is_complete()` is false (node 2 stays Pending forever).
        assert!(run.batch_drained());
        assert!(run.ready_batch().is_empty());
        assert!(!run.is_complete());
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
        let run = WorkflowRun::resume(&spec, "sess", &[node_agent_id(0)]).unwrap();
        // only the remaining worker (node 1) is ready; node 0 is already complete, synth still gated.
        assert_eq!(run.ready_batch(), vec![1]);
        assert!(!run.is_complete());
    }

    #[test]
    fn resume_with_all_done_completes() {
        let spec = fanout_synthesize(vec![RuntimeTask::new("w0")], RuntimeTask::new("synth"));
        // both nodes (worker 0, synth 1) recovered as done.
        let run = WorkflowRun::resume(&spec, "sess", &[node_agent_id(0), node_agent_id(1)]).unwrap();
        assert!(run.ready_batch().is_empty());
        assert!(run.is_complete());
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

    // ── Tournament node (A#2) ───────────────────────────────────────────────────────────────────

    use crate::orchestration::workflow::{NodeKind, WorkflowNode, WorkflowSpec};
    use crate::types::agent::AgentRole;

    /// A 4-entrant tournament controller (node 0) gating a dependent (node 1). Drives the whole
    /// bracket: 4 entrants generate, then 2 round-1 judges, then 1 final judge — and only then does
    /// the dependent unblock, carrying the champion in the controller's `tournament_winner`.
    #[test]
    fn tournament_runs_bracket_then_promotes_dependent() {
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("pick the best ad"), AgentRole::Plan).with_tournament(
                vec![
                    RuntimeTask::new("ad A"),
                    RuntimeTask::new("ad B"),
                    RuntimeTask::new("ad C"),
                    RuntimeTask::new("ad D"),
                ],
            ),
            WorkflowNode::new(RuntimeTask::new("ship the winner"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();

        // Round 1 of spawning expands the controller into 4 entrant children (nodes 2..=5); the
        // controller spawns no agent of its own and the dependent stays gated.
        let entrants = spawn_round(&mut run);
        let entrant_nodes: Vec<usize> = entrants.iter().map(|(n, _)| *n).collect();
        assert_eq!(entrant_nodes, vec![2, 3, 4, 5], "4 entrant children, no controller spawn");
        assert!(run.spawn_info(2).judge_match.is_none(), "entrants are not judges");
        assert!(!run.is_complete());

        // All entrants generate → bracket begins; nothing else spawns until they're all in.
        for (i, (node, id)) in entrants.iter().enumerate() {
            run.record_completion(id, done());
            if i < 3 {
                assert!(run.ready_batch().is_empty(), "no judges until every entrant is in");
            }
            let _ = node;
        }

        // Round 1 judges: 2 matches over the 4 entrants, each carrying its pair.
        let r1 = spawn_round(&mut run);
        assert_eq!(r1.len(), 2, "two round-1 judges");
        let jm0 = run.spawn_info(r1[0].0).judge_match.expect("judge carries a match");
        assert_eq!(jm0, JudgeMatch { left: node_agent_id(2), right: node_agent_id(3) });
        let jm1 = run.spawn_info(r1[1].0).judge_match.expect("judge carries a match");
        assert_eq!(jm1, JudgeMatch { left: node_agent_id(4), right: node_agent_id(5) });

        // Entrant 2 beats 3; entrant 4 beats 5. Dependent still gated mid-bracket.
        run.record_completion(&r1[0].1, judge_done(&node_agent_id(2)));
        run.record_completion(&r1[1].1, judge_done(&node_agent_id(4)));
        assert!(run.ready_batch().iter().all(|&n| n != 1), "dependent gated until the final");

        // Final round: a single judge over the two survivors.
        let r2 = spawn_round(&mut run);
        assert_eq!(r2.len(), 1, "one final judge");
        let jmf = run.spawn_info(r2[0].0).judge_match.expect("final judge carries a match");
        assert_eq!(jmf, JudgeMatch { left: node_agent_id(2), right: node_agent_id(4) });

        // Entrant 4 wins it all → controller completes with the champion, dependent unblocks.
        run.record_completion(&r2[0].1, judge_done(&node_agent_id(4)));
        let winner = run
            .graph
            .get(0)
            .and_then(|n| n.result.as_ref())
            .and_then(|r| r.tournament_winner.clone());
        assert_eq!(winner.as_deref(), Some(node_agent_id(4).as_str()), "champion recorded");
        assert_eq!(run.ready_batch(), vec![1], "dependent unblocks only after the bracket resolves");

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
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("rank"),
            AgentRole::Plan,
        )
        .with_tournament(vec![
            RuntimeTask::new("x"),
            RuntimeTask::new("y"),
            RuntimeTask::new("z"),
        ])]);
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
        assert_eq!(jm, JudgeMatch { left: node_agent_id(1), right: node_agent_id(3) });
        run.record_completion(&r2[0].1, judge_done(&node_agent_id(3)));
        let winner = run.graph.get(0).and_then(|n| n.result.as_ref()).and_then(|r| r.tournament_winner.clone());
        assert_eq!(winner.as_deref(), Some(node_agent_id(3).as_str()));
        assert!(run.is_complete());
    }

    /// A quarantined tournament keeps its entrant + judge children quarantined, and (being
    /// read-only) they pass the quarantine invariant rather than tripping it.
    #[test]
    fn tournament_children_inherit_controller_trust() {
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("judge untrusted inputs"),
            AgentRole::Plan,
        )
        .quarantined()
        .with_tournament(vec![RuntimeTask::new("a"), RuntimeTask::new("b")])]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();

        let entrants = spawn_round(&mut run);
        for (node, _) in &entrants {
            assert_eq!(run.spawn_info(*node).trust, "quarantined", "entrant inherits quarantine");
            assert!(!run.quarantine_violation(*node), "read-only entrant is quarantine-clean");
        }
        for (_, id) in &entrants {
            run.record_completion(id, done());
        }
        let r1 = spawn_round(&mut run);
        assert_eq!(run.spawn_info(r1[0].0).trust, "quarantined", "judge inherits quarantine");
        assert!(!run.quarantine_violation(r1[0].0));
    }

    /// Sanity: the controller node is itself a Tournament kind and never appears in a spawn batch
    /// (entrants/judges carry the work).
    #[test]
    fn tournament_controller_never_spawns_itself() {
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(RuntimeTask::new("c"), AgentRole::Plan)
            .with_tournament(vec![RuntimeTask::new("a"), RuntimeTask::new("b")])]);
        let mut run = WorkflowRun::new(&spec, "sess").unwrap();
        assert!(matches!(run.nodes[0].kind, NodeKind::Tournament { .. }));
        let first = spawn_round(&mut run);
        assert!(first.iter().all(|(n, _)| *n != 0), "controller node 0 never spawns directly");
    }
}
