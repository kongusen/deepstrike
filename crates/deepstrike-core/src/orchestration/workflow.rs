//! Declarative workflow shapes — the six patterns as composable templates.
//!
//! A [`WorkflowSpec`] is a pure, declarative DAG of [`WorkflowNode`]s, each carrying the
//! per-node execution contract (role / isolation / context inheritance / model hint) that
//! the SDK turns into an `AgentRunSpec` at spawn time. This is the data the template
//! constructors below emit, and the shape a future "orchestration-as-syscall" round will
//! lower into per-step [`crate::syscall::Syscall`]s.
//!
//! Three patterns are template constructors here; the other three already have first-class
//! primitives: [`super::tournament::Tournament`], [`super::loop_until_done::LoopUntilDone`],
//! and the adversarial-verification [`crate::harness::eval_pipeline::EvalPipeline`].
//!
//! Pure: no I/O, no clock, no spawning. Validation reuses [`TaskGraph::topological_sort`].

use serde::{Deserialize, Serialize};

use super::task_graph::TaskGraph;
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance};
use crate::types::error::{DeepStrikeError, Result};
use crate::types::task::{RuntimeTask, TaskLane};

/// One node in a workflow DAG: a task plus the contract its agent runs under.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub task: RuntimeTask,
    pub role: AgentRole,
    pub isolation: AgentIsolation,
    pub context_inheritance: ContextInheritance,
    /// Optional model preference (e.g. "opus" / "sonnet"); the SDK resolves it. See W4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<String>,
    /// Indices into [`WorkflowSpec::nodes`] this node depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<usize>,
}

impl WorkflowNode {
    /// A node with role-default isolation/inheritance and no dependencies.
    pub fn new(task: RuntimeTask, role: AgentRole) -> Self {
        let (isolation, context_inheritance) = role_defaults(role);
        Self {
            task,
            role,
            isolation,
            context_inheritance,
            model_hint: None,
            depends_on: Vec::new(),
        }
    }

    pub fn with_depends_on(mut self, depends_on: Vec<usize>) -> Self {
        self.depends_on = depends_on;
        self
    }

    pub fn with_isolation(mut self, isolation: AgentIsolation) -> Self {
        self.isolation = isolation;
        self
    }

    pub fn with_context_inheritance(mut self, inheritance: ContextInheritance) -> Self {
        self.context_inheritance = inheritance;
        self
    }

    pub fn with_model_hint(mut self, hint: impl Into<String>) -> Self {
        self.model_hint = Some(hint.into());
        self
    }
}

/// Role-appropriate defaults for a freshly templated node. Verifiers/explorers run
/// read-only with minimal inherited context to resist self-preferential bias.
fn role_defaults(role: AgentRole) -> (AgentIsolation, ContextInheritance) {
    match role {
        AgentRole::Explore => (AgentIsolation::ReadOnly, ContextInheritance::SystemOnly),
        AgentRole::Verify => (AgentIsolation::ReadOnly, ContextInheritance::None),
        AgentRole::Plan => (AgentIsolation::Shared, ContextInheritance::Full),
        AgentRole::Implement => (AgentIsolation::Worktree, ContextInheritance::Full),
        AgentRole::Custom => (AgentIsolation::Shared, ContextInheritance::None),
    }
}

/// A declarative workflow DAG.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub nodes: Vec<WorkflowNode>,
}

impl WorkflowSpec {
    pub fn new(nodes: Vec<WorkflowNode>) -> Self {
        Self { nodes }
    }

    /// Validate dependency indices are in range and the graph is acyclic.
    pub fn validate(&self) -> Result<()> {
        let n = self.nodes.len();
        for (i, node) in self.nodes.iter().enumerate() {
            for &dep in &node.depends_on {
                if dep >= n {
                    return Err(DeepStrikeError::InvalidConfig(format!(
                        "node {i} depends on out-of-range node {dep} (have {n})"
                    )));
                }
                if dep == i {
                    return Err(DeepStrikeError::InvalidConfig(format!(
                        "node {i} depends on itself"
                    )));
                }
            }
        }
        // Reuse the executor's cycle detection.
        self.to_task_graph()?.topological_sort().map(|_| ())
    }

    /// Lower into an executable [`TaskGraph`] (preserves node order as task ids).
    pub fn to_task_graph(&self) -> Result<TaskGraph> {
        let n = self.nodes.len();
        let mut graph = TaskGraph::new();
        for node in &self.nodes {
            if let Some(&bad) = node.depends_on.iter().find(|&&d| d >= n) {
                return Err(DeepStrikeError::InvalidConfig(format!(
                    "dependency index {bad} out of range (have {n})"
                )));
            }
            graph.add(node.task.clone(), node.depends_on.clone());
        }
        Ok(graph)
    }
}

// ---------------------------------------------------------------------------
// Pattern 1 — Fan-out-and-synthesize
// ---------------------------------------------------------------------------

/// N parallel workers feeding a single synthesize barrier that depends on all of them.
///
/// Workers run as read-only `Explore` agents in the `Retrieve` lane (parallelisable, each
/// with its own clean context); the synthesizer is a `Plan` agent that merges their
/// structured outputs.
pub fn fanout_synthesize(workers: Vec<RuntimeTask>, synthesize: RuntimeTask) -> WorkflowSpec {
    let mut nodes: Vec<WorkflowNode> = workers
        .into_iter()
        .map(|t| WorkflowNode::new(t.with_lane(TaskLane::Retrieve), AgentRole::Explore))
        .collect();
    let worker_ids: Vec<usize> = (0..nodes.len()).collect();
    nodes.push(
        WorkflowNode::new(synthesize.with_lane(TaskLane::Orchestrate), AgentRole::Plan)
            .with_depends_on(worker_ids),
    );
    WorkflowSpec::new(nodes)
}

// ---------------------------------------------------------------------------
// Pattern 2 — Generate-and-filter
// ---------------------------------------------------------------------------

/// N parallel generators feeding a single filter/dedupe step that depends on all of them.
///
/// Structurally a fan-out barrier, but semantically distinct: generators are `Implement`
/// agents producing candidates; the filter is a `Verify` agent that ranks/dedupes against
/// a rubric (pair with [`crate::harness::eval_pipeline::EvalPipeline`] for the rubric).
pub fn generate_and_filter(generators: Vec<RuntimeTask>, filter: RuntimeTask) -> WorkflowSpec {
    let mut nodes: Vec<WorkflowNode> = generators
        .into_iter()
        .map(|t| WorkflowNode::new(t.with_lane(TaskLane::Retrieve), AgentRole::Implement))
        .collect();
    let gen_ids: Vec<usize> = (0..nodes.len()).collect();
    nodes.push(
        WorkflowNode::new(filter.with_lane(TaskLane::Verify), AgentRole::Verify)
            .with_depends_on(gen_ids),
    );
    WorkflowSpec::new(nodes)
}

// ---------------------------------------------------------------------------
// W2 — Adversarial verification (the default contract)
// ---------------------------------------------------------------------------

/// One fresh-context verifier per rule/claim, optionally followed by a skeptic that re-checks
/// every flag to suppress false positives.
///
/// This is the article's rule-adherence pattern. Each verifier runs as a `Verify` agent, which
/// [`role_defaults`] gives `ReadOnly` isolation + [`ContextInheritance::None`] — the verifier does
/// **not** inherit the author's reasoning, so it cannot rubber-stamp it (the structural defence
/// against self-preferential bias). The optional `skeptic` depends on all verifiers and reviews
/// their flags (real violation vs. false positive). Runs on the W0 workflow executor.
///
/// Pair each verifier with [`crate::harness::eval_pipeline::EvalPipeline`] at run time for the
/// rubric scoring. For rules known only at run time (claim extraction), a dynamic-fan-out variant
/// is a later round; this covers the case where the rule/claim set is known up front.
pub fn verify_rules(rules: Vec<RuntimeTask>, skeptic: Option<RuntimeTask>) -> WorkflowSpec {
    let mut nodes: Vec<WorkflowNode> = rules
        .into_iter()
        .map(|t| WorkflowNode::new(t.with_lane(TaskLane::Verify), AgentRole::Verify))
        .collect();
    if let Some(skeptic) = skeptic {
        let verifier_ids: Vec<usize> = (0..nodes.len()).collect();
        nodes.push(
            WorkflowNode::new(skeptic.with_lane(TaskLane::Verify), AgentRole::Verify)
                .with_depends_on(verifier_ids),
        );
    }
    WorkflowSpec::new(nodes)
}

// ---------------------------------------------------------------------------
// Pattern 3 — Classify-and-act
// ---------------------------------------------------------------------------

/// A classifier followed by labeled branches, exactly one of which runs.
///
/// This is **conditional**, so it is not a static DAG: the SDK runs the classifier, reads
/// its label, then [`route`](ClassifyAndAct::route)s to the single branch to spawn. The
/// kernel-side part is the routing table — no I/O.
#[derive(Debug, Clone)]
pub struct ClassifyAndAct {
    pub classifier: WorkflowNode,
    /// `(label, action)` branches; `route` matches a classifier label to its action.
    pub branches: Vec<(String, WorkflowNode)>,
}

impl ClassifyAndAct {
    /// Return the branch action for a classifier label, if one matches.
    pub fn route(&self, label: &str) -> Option<&WorkflowNode> {
        self.branches
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, node)| node)
    }
}

/// Build a classify-and-act workflow: a `Plan` classifier plus labeled `Implement` branches.
pub fn classify_and_act(
    classifier: RuntimeTask,
    branches: Vec<(String, RuntimeTask)>,
) -> ClassifyAndAct {
    ClassifyAndAct {
        classifier: WorkflowNode::new(classifier, AgentRole::Plan),
        branches: branches
            .into_iter()
            .map(|(label, task)| (label, WorkflowNode::new(task, AgentRole::Implement)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(goal: &str) -> RuntimeTask {
        RuntimeTask::new(goal)
    }

    #[test]
    fn fanout_synthesize_shape() {
        let spec = fanout_synthesize(
            vec![task("search A"), task("search B"), task("search C")],
            task("merge findings"),
        );
        assert_eq!(spec.nodes.len(), 4);
        // synthesize node depends on all three workers
        assert_eq!(spec.nodes[3].depends_on, vec![0, 1, 2]);
        assert_eq!(spec.nodes[3].role, AgentRole::Plan);
        assert_eq!(spec.nodes[0].role, AgentRole::Explore);
        assert_eq!(spec.nodes[0].isolation, AgentIsolation::ReadOnly);
        spec.validate().unwrap();
        // workers are the only ready tasks before any completion
        let graph = spec.to_task_graph().unwrap();
        assert_eq!(graph.ready_tasks(), vec![0, 1, 2]);
    }

    #[test]
    fn generate_and_filter_shape() {
        let spec = generate_and_filter(vec![task("idea 1"), task("idea 2")], task("dedupe + rank"));
        assert_eq!(spec.nodes.len(), 3);
        assert_eq!(spec.nodes[2].depends_on, vec![0, 1]);
        assert_eq!(spec.nodes[2].role, AgentRole::Verify);
        assert_eq!(spec.nodes[2].context_inheritance, ContextInheritance::None);
        assert_eq!(spec.nodes[0].role, AgentRole::Implement);
        spec.validate().unwrap();
    }

    #[test]
    fn verify_rules_with_skeptic_shape() {
        let spec = verify_rules(
            vec![task("money is integer cents"), task("errors propagate"), task("utc timestamps")],
            Some(task("skeptic: real violation or false positive?")),
        );
        assert_eq!(spec.nodes.len(), 4);
        // skeptic depends on every verifier
        assert_eq!(spec.nodes[3].depends_on, vec![0, 1, 2]);
        assert_eq!(spec.nodes[3].role, AgentRole::Verify);
        spec.validate().unwrap();
        // verifiers are the ready set; skeptic gated behind them
        assert_eq!(spec.to_task_graph().unwrap().ready_tasks(), vec![0, 1, 2]);
    }

    #[test]
    fn verify_rules_verifiers_are_bias_resistant() {
        // The default contract: every verifier runs with no inherited author context.
        let spec = verify_rules(vec![task("rule a"), task("rule b")], None);
        assert_eq!(spec.nodes.len(), 2); // no skeptic → just the verifiers
        for node in &spec.nodes {
            assert_eq!(node.role, AgentRole::Verify);
            assert_eq!(node.context_inheritance, ContextInheritance::None);
            assert_eq!(node.isolation, AgentIsolation::ReadOnly);
            assert!(node.depends_on.is_empty()); // all parallel
        }
        spec.validate().unwrap();
    }

    #[test]
    fn verify_rules_empty_with_skeptic_is_just_skeptic() {
        // No rules → skeptic has nothing to depend on; still a valid single-node spec.
        let spec = verify_rules(vec![], Some(task("skeptic")));
        assert_eq!(spec.nodes.len(), 1);
        assert!(spec.nodes[0].depends_on.is_empty());
        spec.validate().unwrap();
    }

    #[test]
    fn classify_and_act_routes() {
        let c = classify_and_act(
            task("classify the ticket"),
            vec![
                ("bug".into(), task("attempt fix")),
                ("question".into(), task("answer it")),
            ],
        );
        assert_eq!(c.classifier.role, AgentRole::Plan);
        assert_eq!(c.route("bug").unwrap().task.goal, "attempt fix");
        assert_eq!(c.route("question").unwrap().task.goal, "answer it");
        assert!(c.route("unknown").is_none());
    }

    #[test]
    fn validate_rejects_out_of_range_dep() {
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(task("a"), AgentRole::Explore),
            WorkflowNode::new(task("b"), AgentRole::Plan).with_depends_on(vec![5]),
        ]);
        assert!(spec.validate().is_err());
    }

    #[test]
    fn validate_rejects_self_dependency() {
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(task("a"), AgentRole::Plan).with_depends_on(vec![0]),
        ]);
        assert!(spec.validate().is_err());
    }

    #[test]
    fn validate_rejects_cycle() {
        // 0 -> 1 -> 0 forms a cycle (both reference each other)
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(task("a"), AgentRole::Plan).with_depends_on(vec![1]),
            WorkflowNode::new(task("b"), AgentRole::Plan).with_depends_on(vec![0]),
        ]);
        assert!(spec.validate().is_err());
    }

    #[test]
    fn node_builder_overrides_defaults() {
        let n = WorkflowNode::new(task("x"), AgentRole::Verify)
            .with_isolation(AgentIsolation::Worktree)
            .with_model_hint("opus");
        assert_eq!(n.isolation, AgentIsolation::Worktree);
        assert_eq!(n.model_hint.as_deref(), Some("opus"));
        // default inheritance for Verify is None (bias-resistant)
        assert_eq!(n.context_inheritance, ContextInheritance::None);
    }
}
