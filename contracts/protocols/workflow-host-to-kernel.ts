/** Workflow definition projections used by the live start and submit paths. */

import type { BoundaryProtocol } from "./types.js"

export const WORKFLOW_HOST_TO_KERNEL_PROTOCOL: BoundaryProtocol = {
  id: "workflow.host-to-kernel",
  family: "host-to-kernel",
  direction: "project",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "types/agent:workflowNodeSpecToKernel",
      source: { type: "WorkflowNodeSpec", layer: "host", authority: "host-runtime" },
      target: { type: "KernelWorkflowNode", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: ["role", "isolation"],
        renames: {
          contextInheritance: "context_inheritance",
          modelHint: "model_hint",
          outputSchema: "output_schema",
          tokenBudget: "token_budget",
          maxTurns: "max_turns",
          maxWallMs: "max_wall_ms",
          dependsOn: "depends_on",
          depPolicy: "dep_policy",
        },
        drops: ["nodeId", "agent", "context"],
        derived: ["kind"],
        nested: [
          { source: "task", target: "task", kind: "project", note: "Bare goals normalize to RuntimeTask JSON with criteria defaulted." },
          { source: "schedulingFactors", target: "scheduling_factors", kind: "project", note: "Host scheduling inputs are validated and lowered to snake_case kernel fields." },
        ],
        forbidden: [],
      },
    },
    {
      adapter: "types/agent:workflowSpecToKernel",
      source: { type: "WorkflowSpec", layer: "host", authority: "host-runtime" },
      target: { type: "KernelWorkflowSpec", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: [],
        derived: ["nodes"],
        forbidden: [],
      },
    },
  ],
  lossiness: "intentional",
  validation: {
    mode: "behavioral-tests",
    reason: "Workflow ABI and control-flow tests cover node lowering, snake_case fields, and root composition.",
    testRefs: ["node/tests/workflow-boundary.test.ts", "node/tests/workflow-control-flow.test.ts", "node/tests/workflow-optimization.test.ts"],
  },
}
