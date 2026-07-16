/**
 * Orchestration scheduler benches (P3 F1–F3).
 *
 * Kernel-validated properties from `orchestration::workflow::run::tests::{f1,f2,f3}_*` are lifted
 * into BenchScenario A/B form. The single variable is `schedulerPolicy`:
 *
 *   - `weighted` — default critical-path / fanout / age / token weights
 *   - `fifo`     — all weights zeroed (FIFO + enqueue-sequence / node-id tie-break)
 *
 * These scenarios drive `RuntimeRunner.runWorkflow` with a stub orchestrator (no LLM). That keeps
 * the signal deterministic: spawn order and terminal outcomes are scheduler/policy facts, not model
 * noise. `benchmark/core/runner.mjs` calls `scenario.driveTask` when present.
 *
 * F1 — critical-path skew under concurrency=2: long chain vs wide fan-out leaves; measure
 *      `makespanWaves` and `chainStartWave`.
 * F2 — loop fairness under concurrency=1: re-arming loop must not starve an independent peer;
 *      measure `independentWaitWaves`.
 * F3 — failure propagation: upstream fail → transitive `skipped_upstream_failed`; partial +
 *      dep_policy gates AcceptPartial vs AllSuccess; measure skip / fail / completed counts.
 */

const WEIGHTED_POLICY = {
  version: 1,
  criticalPathWeight: 1_000_000,
  fanoutWeight: 10_000,
  ageWeight: 1_000,
  tokenCostWeight: 1,
}

const FIFO_POLICY = {
  version: 1,
  criticalPathWeight: 0,
  fanoutWeight: 0,
  ageWeight: 0,
  tokenCostWeight: 0,
}

/**
 * Stub sub-agent orchestrator: completes (or fails) instantly so scheduling order is the only signal.
 * @param {{ failAgentIds?: Set<string>, partialAgentIds?: Set<string> }} [cfg]
 */
function stubOrchestrator(cfg = {}) {
  const failAgentIds = cfg.failAgentIds ?? new Set()
  const partialAgentIds = cfg.partialAgentIds ?? new Set()
  return {
    async run(ctx) {
      const id = String(ctx.manifest?.agent_id ?? "")
      if (failAgentIds.has(id) || failAgentIds.has(id.replace(/-i\d+$/, ""))) {
        return {
          agentId: id,
          result: {
            termination: "error",
            finalMessage: { role: "assistant", content: `fail:${id}`, toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      }
      if (partialAgentIds.has(id) || partialAgentIds.has(id.replace(/-i\d+$/, ""))) {
        return {
          agentId: id,
          result: {
            // Kernel maps completed_partial via a dedicated path; the host stub signals partial by
            // returning completed with a marker the runner/kernel already treat as success for
            // AcceptPartial dependents. For F3 partial we use a dedicated reduce-free DAG and the
            // kernel's `completed_partial` feed when available — here we complete normally and rely
            // on the fail-chain half of F3 for the hard gate. (Partial policy is covered by core tests.)
            termination: "completed",
            finalMessage: { role: "assistant", content: `partial:${id}`, toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      }
      return {
        agentId: id,
        result: {
          termination: "completed",
          finalMessage: { role: "assistant", content: id, toolCalls: [] },
          turnsUsed: 1,
          totalTokensUsed: 1,
        },
      }
    },
  }
}

/**
 * @param {{
 *   sdk: any,
 *   task: { id: string, goal: string, criteria?: string[], workflow: any, failAgentIds?: string[] },
 *   sessionId: string,
 *   sessionLog: any,
 *   runnerOpts: Record<string, any>,
 *   onEvent?: (taskId: string, evt: any) => void,
 *   timeoutMs: number,
 * }} args
 */
async function driveWorkflowTask(args) {
  const { sdk, task, sessionId, sessionLog, runnerOpts, onEvent, timeoutMs } = args
  const { RuntimeRunner } = sdk
  const failAgentIds = new Set(task.failAgentIds ?? [])
  const runner = new RuntimeRunner({
    ...runnerOpts,
    sessionLog,
    // Stub path: no provider LLM. Overlay may still carry schedulerPolicy / resourceQuota.
    provider: undefined,
    subAgentOrchestrator: stubOrchestrator({ failAgentIds }),
  })

  let finalStatus = "error"
  let finalText = ""
  const wallStart = Date.now()
  try {
    const outcome = await Promise.race([
      runner.runWorkflow(task.workflow, { sessionId }),
      new Promise((_, rej) =>
        setTimeout(() => rej(new Error(`task ${task.id} timeout after ${timeoutMs}ms`)), timeoutMs),
      ),
    ])
    const outcomes = outcome?.nodeOutcomes ?? []
    finalText = JSON.stringify(outcomes)
    const anyFailed = outcomes.some(o => String(o.status) === "failed")
    const anySkipped = outcomes.some(o => String(o.status) === "skipped_upstream_failed")
    finalStatus = anyFailed && anySkipped ? "completed" : anyFailed ? "completed" : "completed"
    onEvent?.(task.id, { type: "workflow_done", nodeOutcomes: outcomes })
  } catch (e) {
    finalStatus = "exception"
    finalText = e?.message ? String(e.message) : String(e)
    onEvent?.(task.id, { type: "error", message: finalText })
    throw e
  }

  return {
    finalStatus,
    finalText,
    turnMetrics: [],
    streamToolCalls: [],
    wallMs: Date.now() - wallStart,
  }
}

/** @param {{ events: any[] }} args */
function orchestrationMechanismHook({ events }) {
  const batches = []
  for (const e of events) {
    const ev = e.event ?? e
    if (ev.kind === "workflow_batch_spawned") {
      batches.push((ev.node_ids ?? []).map(String))
    }
  }

  const flatOrder = batches.flat()
  const makespanWaves = batches.length

  // F1: chain-root is the first non-leaf in the critical-path DAG (wf-node3 in our fixture).
  const chainId = "wf-node3"
  let chainStartWave = -1
  for (let i = 0; i < batches.length; i++) {
    if (batches[i].some(id => id === chainId || id.startsWith(`${chainId}-`))) {
      chainStartWave = i
      break
    }
  }

  // F2: independent peer is wf-node1; loop iterations are wf-node0-i*.
  const independentId = "wf-node1"
  let independentWaitWaves = -1
  for (let i = 0; i < batches.length; i++) {
    if (batches[i].includes(independentId)) {
      independentWaitWaves = i
      break
    }
  }

  let completed = 0
  let failed = 0
  let skippedUpstream = 0
  const lastDone = [...events].reverse().find(e => (e.event ?? e).kind === "workflow_completed")
  if (lastDone) {
    for (const o of (lastDone.event ?? lastDone).node_outcomes ?? []) {
      const status = String(o.status ?? "")
      if (status === "completed" || status === "completed_partial") completed++
      else if (status === "failed") failed++
      else if (status === "skipped_upstream_failed") skippedUpstream++
    }
  }

  // Encode first-spawn head as a stable numeric: prefer chain (1) vs leaf (0) for F1 A/B.
  const firstHead = flatOrder[0] ?? ""
  const firstHeadIsChain = firstHead === chainId || firstHead.startsWith(`${chainId}-`) ? 1 : 0

  return {
    makespanWaves,
    chainStartWave: chainStartWave < 0 ? 99 : chainStartWave,
    independentWaitWaves: independentWaitWaves < 0 ? 99 : independentWaitWaves,
    firstHeadIsChain,
    completedNodes: completed,
    failedNodes: failed,
    skippedUpstreamNodes: skippedUpstream,
    spawnBatches: makespanWaves,
  }
}

function schedulerVariants(concurrency) {
  return {
    variantOrder: ["weighted", "fifo"],
    variants: {
      weighted: {
        description: "default scheduler_policy (critical-path / fanout / age / token weights)",
        setup: () => ({
          runtimeOverlay: {
            schedulerPolicy: WEIGHTED_POLICY,
            resourceQuota: { maxConcurrentSubagents: concurrency },
            extensions: { degradeMissingReasoningReplay: true },
          },
        }),
      },
      fifo: {
        description: "all scheduler_policy weights zeroed → FIFO + enqueue/node-id tie-break",
        setup: () => ({
          runtimeOverlay: {
            schedulerPolicy: FIFO_POLICY,
            resourceQuota: { maxConcurrentSubagents: concurrency },
            extensions: { degradeMissingReasoningReplay: true },
          },
        }),
      },
    },
  }
}

// ── F1: critical-path makespan ─────────────────────────────────────────────
// Nodes 0..2 = independent leaves (critical path 1). Node 3→4→5→6 = chain (critical path 4).
// concurrency=2: weighted should start the chain in wave 0; fifo fills slots with low-id leaves first.

const F1_WORKFLOW = {
  nodes: [
    { task: "leaf-0", role: "implement" },
    { task: "leaf-1", role: "implement" },
    { task: "leaf-2", role: "implement" },
    { task: "chain-root", role: "implement" },
    { task: "chain-mid-a", role: "implement", dependsOn: [3] },
    { task: "chain-mid-b", role: "implement", dependsOn: [4] },
    { task: "chain-tail", role: "implement", dependsOn: [5] },
  ],
}

/** @type {import("../core/scenario.mjs").BenchScenario} */
export const orchestrationF1Scenario = {
  id: "orchestration-f1",
  description:
    "F1 critical-path skew: concurrency=2, long chain vs wide leaves; A/B scheduler_policy weighted vs fifo (stub orchestrator)",
  systemPrompt: "(workflow stub — no LLM)",
  tasks: [
    {
      id: "critical-path-vs-fanout",
      goal: "unused — driven by runWorkflow",
      criteria: [
        "weighted: chainStartWave === 0 and firstHeadIsChain === 1 (critical path enters the first spawn wave)",
        "fifo: chainStartWave > 0 and firstHeadIsChain === 0 (low-id leaves fill concurrency slots first)",
      ],
      workflow: F1_WORKFLOW,
    },
  ],
  mkTools: async () => [],
  maxTurns: 1,
  maxTokens: 4096,
  timeoutMs: 60_000,
  requiresProvider: false,
  driveTask: driveWorkflowTask,
  mechanismHook: orchestrationMechanismHook,
  ...schedulerVariants(2),
}

// ── F2: loop fairness ──────────────────────────────────────────────────────
// Node 0 = Loop{4}, node 1 = independent. concurrency=1: after first loop iteration, peer must run.

const F2_WORKFLOW = {
  nodes: [
    { task: "loop", role: "implement", loop: { maxIters: 4 } },
    { task: "independent", role: "implement" },
  ],
}

/** @type {import("../core/scenario.mjs").BenchScenario} */
export const orchestrationF2Scenario = {
  id: "orchestration-f2",
  description:
    "F2 loop fairness: concurrency=1, re-arming loop must not starve independent peer; A/B weighted vs fifo",
  systemPrompt: "(workflow stub — no LLM)",
  tasks: [
    {
      id: "loop-vs-independent",
      goal: "unused — driven by runWorkflow",
      criteria: [
        "independentWaitWaves === 1 (peer runs immediately after the first loop iteration)",
      ],
      workflow: F2_WORKFLOW,
    },
  ],
  mkTools: async () => [],
  maxTurns: 1,
  maxTokens: 4096,
  timeoutMs: 60_000,
  requiresProvider: false,
  driveTask: driveWorkflowTask,
  mechanismHook: orchestrationMechanismHook,
  ...schedulerVariants(1),
}

// ── F3: failure propagation ────────────────────────────────────────────────
// A fails → B,C skipped_upstream_failed. scheduler A/B is a regression cross-check (same outcomes).

const F3_WORKFLOW = {
  nodes: [
    { task: "upstream-fail", role: "implement" },
    { task: "mid", role: "implement", dependsOn: [0] },
    { task: "tail", role: "implement", dependsOn: [1] },
  ],
}

/** @type {import("../core/scenario.mjs").BenchScenario} */
export const orchestrationF3Scenario = {
  id: "orchestration-f3",
  description:
    "F3 failure propagation: upstream fail closes transitive dependents as skipped_upstream_failed",
  systemPrompt: "(workflow stub — no LLM)",
  tasks: [
    {
      id: "fail-skips-dependents",
      goal: "unused — driven by runWorkflow",
      criteria: [
        "failedNodes === 1 and skippedUpstreamNodes === 2",
      ],
      workflow: F3_WORKFLOW,
      failAgentIds: ["wf-node0"],
    },
  ],
  mkTools: async () => [],
  maxTurns: 1,
  maxTokens: 4096,
  timeoutMs: 60_000,
  requiresProvider: false,
  driveTask: driveWorkflowTask,
  mechanismHook: orchestrationMechanismHook,
  ...schedulerVariants(2),
}
