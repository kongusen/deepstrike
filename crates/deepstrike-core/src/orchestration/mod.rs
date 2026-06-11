pub mod executor;
pub mod gen_eval;
pub mod planner;
pub mod task_graph;
/// Single-elimination bracket — **kernel-internal**: the shared bracket core behind
/// [`workflow::NodeKind::Tournament`]. No longer an SDK-exposed standalone primitive (A#1).
pub mod tournament;
pub mod workflow;
