/**
 * Scenario: orchestration-f2 (P3 F2 — loop fairness / no starvation).
 *
 * DAG: Loop{5} (node 0) + independent peer (node 1). Concurrency = 1.
 * After the loop's first iteration re-arms, the peer (waiting since wave 0) must
 * run before the loop's second iteration — under both weighted and fifo (enqueue
 * FIFO tie-break). mechanismHook pins `independentNotStarved=1`.
 */

import {
  WEIGHTED_POLICY,
  FIFO_POLICY,
  driveWorkflowTask,
  orchestrationMechanismHook,
  schedulerOverlay,
  mkEmptyTools,
} from "./orchestration-shared.mjs"

const INDEPENDENT = "wf-node1"

const WORKFLOW = {
  nodes: [
    { task: "loop-body", role: "implement", loop: { maxIters: 5 } },
    { task: "independent-peer", role: "implement" },
  ],
}

/** @type {import("../core/scenario.mjs").BenchScenario} */
export const orchestrationF2Scenario = {
  id: "orchestration-f2",
  description:
    "F2 loop fairness: Loop{5} + independent peer, concurrency=1; peer must not starve",
  systemPrompt: "deterministic workflow stub — no LLM",
  tasks: [
    {
      id: "loop-vs-independent",
      goal: "scheduler F2 fairness",
      criteria: ["independent peer starts by wave 1"],
      workflow: WORKFLOW,
    },
  ],
  mkTools: mkEmptyTools,
  maxTurns: 1,
  maxTokens: 4096,
  timeoutMs: 60_000,
  driveTask: driveWorkflowTask,
  mechanismHook: args =>
    orchestrationMechanismHook(args, { expectIndependentId: INDEPENDENT }),

  variantOrder: ["weighted", "fifo"],
  variants: {
    weighted: {
      description: "default scheduler_policy (age weight helps older waiters)",
      setup: () => ({ runtimeOverlay: schedulerOverlay(WEIGHTED_POLICY, 1) }),
    },
    fifo: {
      description: "zero weights — enqueue-sequence FIFO must still yield to the peer",
      setup: () => ({ runtimeOverlay: schedulerOverlay(FIFO_POLICY, 1) }),
    },
  },
}
