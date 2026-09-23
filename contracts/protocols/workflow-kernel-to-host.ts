/** Workflow kernel action projections consumed by the host runner. */

import type { BoundaryProtocol } from "./types.js"

export const WORKFLOW_KERNEL_TO_HOST_PROTOCOL: BoundaryProtocol = {
  id: "workflow.kernel-to-host",
  family: "kernel-to-host",
  direction: "decode",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "runtime/kernel-step:workflowSpawnNodeFromKernel",
      source: { type: "KernelWorkflowSpawnNode", layer: "kernel", authority: "kernel" },
      target: { type: "WorkflowSpawnInfo", layer: "host", authority: "host-runtime" },
      fields: {
        preserves: [
          "agent_id",
          "goal",
          "role",
          "isolation",
          "context_inheritance",
          "model_hint",
          "trust",
          "output_schema",
          "reducer",
          "input_agent_ids",
          "dependency_outputs",
          "judge_match",
          "loop_max_iters",
          "classify_labels",
          "token_budget",
          "max_turns",
          "max_wall_ms",
        ],
        drops: ["task_id", "attempt_id", "launch_token", "node_id"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/kernel-step:workflowBudgetFromKernel",
      source: { type: "KernelWorkflowBudget", layer: "kernel", authority: "kernel" },
      target: { type: "WorkflowBudget", layer: "host", authority: "host-runtime" },
      fields: {
        preserves: [
          "nodes_used",
          "nodes_max",
          "nodes_remaining",
          "running_subagents",
          "max_concurrent_subagents",
          "concurrency_remaining",
          "tokens_used",
          "tokens_max",
          "tokens_remaining",
          "max_total_tokens",
          "max_turns",
          "max_concurrency",
        ],
        forbidden: [],
      },
    },
  ],
  lossiness: "intentional",
  validation: {
    mode: "behavioral-tests",
    reason: "Workflow action boundary tests cover kernel bookkeeping drops and host runner projections.",
  },
}
