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
    {
      adapter: "workflow/dynamic:dynamicWorkflowPlanToKernel",
      source: { type: "DynamicWorkflowPlan", layer: "host", authority: "host-runtime" },
      target: { type: "KernelDynamicWorkflowPlan", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: ["sequence"],
        renames: {
          runId: "run_id",
        },
        nested: [
          { source: "nodes.[].nodeId", target: "nodes.[].node_id", kind: "project" },
          { source: "nodes.[].dependsOn", target: "nodes.[].depends_on", kind: "project" },
          { source: "nodes.[].promptFingerprint", target: "nodes.[].prompt_fingerprint", kind: "project" },
          { source: "nodes.[].replay", target: "nodes.[].replay", kind: "project" },
        ],
        drops: [],
        derived: [],
        forbidden: [],
      },
    },
    {
      adapter: "workflow/dynamic:dynamicWorkflowReplayFactToKernel",
      source: { type: "DynamicWorkflowReplayFact", layer: "host", authority: "host-runtime" },
      target: { type: "KernelDynamicWorkflowReplayFact", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: ["sequence", "status", "replay"],
        renames: {
          runId: "run_id",
          nodeId: "node_id",
          promptFingerprint: "prompt_fingerprint",
          resultDigest: "result_digest",
        },
        drops: [],
        derived: [],
        forbidden: [],
      },
    },
  ],
  lossiness: "intentional",
  validation: {
    mode: "behavioral-tests",
    reason: "Workflow ABI and control-flow tests cover node lowering, snake_case fields, and root composition.",
    testRefs: [{ path: "node/tests/workflow-boundary.test.ts", selectors: ["workflow host-to-kernel boundary"] }, { path: "node/tests/workflow-control-flow.test.ts", selectors: ["workflowNodeSpecToKernel: control-flow kinds"] }, { path: "node/tests/workflow-optimization.test.ts", selectors: ["W-N2 / W-N7: spawn descriptors carry data edges and per-node caps"] }],
  },
}
