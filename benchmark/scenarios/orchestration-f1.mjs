/**
 * Scenario: orchestration-f1 (P3 F1 — critical-path skew / makespan).
 *
 * DAG: three low-id leaves + a length-4 chain. Concurrency = 2.
 *   weighted — chain-root enters the first spawn wave → fewer waves to finish
 *   fifo     — lower-id leaves fill slots first → chain starts later → more waves
 */

import {
  WEIGHTED_POLICY,
  FIFO_POLICY,
  driveWorkflowTask,
  orchestrationMechanismHook,
  schedulerOverlay,
  mkEmptyTools,
} from "./orchestration-shared.mjs"

const CHAIN_ROOT = "wf-node3"

const WORKFLOW = {
  nodes: [
    { task: "leaf-0", role: "implement" },
    { task: "leaf-1", role: "implement" },
    { task: "leaf-2", role: "implement" },
    { task: "chain-root", role: "implement" },
    { task: "chain-1", role: "implement", dependsOn: [3] },
    { task: "chain-2", role: "implement", dependsOn: [4] },
    { task: "chain-3", role: "implement", dependsOn: [5] },
  ],
}

/** @type {import("../core/scenario.mjs").BenchScenario} */
export const orchestrationF1Scenario = {
  id: "orchestration-f1",
  description:
    "F1 critical-path skew: 3 leaves + length-4 chain, concurrency=2; A/B weighted vs fifo makespan",
  systemPrompt: "deterministic workflow stub — no LLM",
  tasks: [
    {
      id: "critical-path-vs-fanout",
      goal: "scheduler F1 makespan",
      criteria: ["chain starts in first wave under weighted policy"],
      workflow: WORKFLOW,
    },
  ],
  mkTools: mkEmptyTools,
  maxTurns: 1,
  maxTokens: 4096,
  timeoutMs: 60_000,
  driveTask: driveWorkflowTask,
  mechanismHook: args =>
    orchestrationMechanismHook(args, { expectChainId: CHAIN_ROOT }),

  variantOrder: ["weighted", "fifo"],
  variants: {
    weighted: {
      description: "default scheduler_policy (critical-path / fanout / age weights)",
      setup: () => ({ runtimeOverlay: schedulerOverlay(WEIGHTED_POLICY, 2) }),
    },
    fifo: {
      description: "all scheduler weights zeroed → FIFO / node-id order",
      setup: () => ({ runtimeOverlay: schedulerOverlay(FIFO_POLICY, 2) }),
    },
  },
}
