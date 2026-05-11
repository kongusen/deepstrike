use super::task_graph::TaskGraph;
use crate::types::message::Message;
use crate::types::result::{LoopResult, SubAgentResult};

/// Execution plan: which tasks to run next.
#[derive(Debug)]
pub struct ExecutionPlan {
    /// Task IDs that can run in parallel right now.
    pub runnable: Vec<usize>,
    /// Whether the entire graph is complete.
    pub all_done: bool,
}

/// Stateless executor that inspects the graph and returns what to run next.
/// Actual execution happens in the SDK layer.
pub fn next_batch(graph: &TaskGraph) -> ExecutionPlan {
    let runnable = graph.ready_tasks();
    let all_done = graph.all_done();
    ExecutionPlan { runnable, all_done }
}

/// Report a task result back to the graph.
pub fn report_completion(graph: &mut TaskGraph, task_id: usize, result: LoopResult) {
    graph.complete(task_id, result);
}

/// Report a sub-agent result back to the graph and return a Message
/// suitable for injection into the parent agent's context.
pub fn report_sub_agent(
    graph: &mut TaskGraph,
    task_id: usize,
    result: SubAgentResult,
) -> Option<Message> {
    let output = result.result.final_message.as_ref().map(|m| {
        let text = m.content.as_text().unwrap_or("[sub-agent completed]");
        Message::user(format!("[sub-agent {}] {}", result.agent_id, text))
    });
    graph.complete(task_id, result.result);
    output
}

/// Report a task failure back to the graph.
pub fn report_failure(graph: &mut TaskGraph, task_id: usize) {
    graph.fail(task_id);
}
