use super::task_graph::TaskGraph;
use crate::types::task::RuntimeTask;

/// Task decomposition result from the SDK layer.
#[derive(Debug, Clone)]
pub struct DecomposedTask {
    pub task: RuntimeTask,
    /// Indices into the decomposition list that this task depends on.
    pub depends_on: Vec<usize>,
}

/// Build a TaskGraph from a list of decomposed tasks in a single pass.
pub fn build_graph(tasks: Vec<DecomposedTask>) -> TaskGraph {
    let mut graph = TaskGraph::new();
    for task in tasks {
        graph.add(task.task, task.depends_on);
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_graph_from_decomposition() {
        let tasks = vec![
            DecomposedTask {
                task: RuntimeTask::new("Setup DB"),
                depends_on: vec![],
            },
            DecomposedTask {
                task: RuntimeTask::new("Run migrations"),
                depends_on: vec![0],
            },
            DecomposedTask {
                task: RuntimeTask::new("Seed data"),
                depends_on: vec![1],
            },
        ];

        let graph = build_graph(tasks);
        assert_eq!(graph.len(), 3);
        assert!(graph.topological_sort().is_ok());
        assert_eq!(graph.ready_tasks(), vec![0]);
    }
}
