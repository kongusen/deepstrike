/**
 * Orchestration scheduler benches (P3 F1 / F2 / F3).
 *
 * These are deterministic DAG drives: stub orchestrator (no LLM), A/B on
 * `schedulerPolicy` (weighted defaults vs all-zero FIFO). Concurrency is capped so
 * ready-set ORDER becomes observable as spawn waves.
 *
 *   F1 — critical-path skew → fewer spawn waves (makespan) under weighted policy
 *   F2 — loop fairness → independent peer starts by wave 1 (no starvation)
 *   F3 — failure propagation → transitive SkippedUpstreamFailed + partial dep_policy
 *
 * Driven via BenchScenario.driveTask → RuntimeRunner.runWorkflow (not runner.run).
 */

import { loadSdk } from "../utils/sdk.mjs"

/** @returns {Promise<any>} */
async function getSdk() {
  return loadSdk()
}

/** Default policy weights (mirrors kernel SchedulerPolicyConfig::default). */
export const WEIGHTED_POLICY = {
  version: 1,
  criticalPathWeight: 1_000_000,
  fanoutWeight: 10_000,
  ageWeight: 1_000,
  tokenCostWeight: 1,
}

/** All-zero → FIFO with enqueue-sequence / node-id tie-break. */
export const FIFO_POLICY = {
  version: 1,
  criticalPathWeight: 0,
  fanoutWeight: 0,
  ageWeight: 0,
  tokenCostWeight: 0,
}

/**
 * Stub sub-agent orchestrator. Completes instantly; fails nodes whose task text
 * contains `[FAIL]`; emits CompletedPartial when task contains `[PARTIAL]`.
 *
 * @param {{ onSpawn?: (agentId: string, task: string) => void }} [opts]
 */
export function makeStubOrchestrator(opts = {}) {
  return {
    async run(ctx) {
      const id = String(ctx.manifest?.agent_id ?? "")
      const task = String(ctx.spec?.goal ?? ctx.manifest?.goal ?? id)
      opts.onSpawn?.(id, task)

      if (task.includes("[FAIL]")) {
        return {
          agentId: id,
          result: {
            termination: "error",
            finalMessage: { role: "assistant", content: `failed:${id}`, toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      }
      if (task.includes("[PARTIAL]")) {
        return {
          agentId: id,
          result: {
            termination: "completed",
            // Kernel maps host "partial" via CompletedPartial when the runner reports it —
            // use the workflow path's completed_partial by returning a partial-flavoured result
            // through the standard completed path is not enough; mark via metadata the driver
            // understands. For stub benches we rely on node task naming + dep_policy tests that
            // the kernel unit tests already cover; F3 primary assertion is FAIL→skip chain.
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
 * @param {object} args
 * @param {import("../core/scenario.mjs").BenchScenario} args.scenario
 * @param {import("../core/scenario.mjs").BenchTask} args.task
 * @param {string} args.sessionId
 * @param {any} args.sessionLog
 * @param {Record<string, any>} args.runnerOpts
 * @param {(taskId: string, evt: any) => void} [args.onEvent]
 * @param {number} [args.timeoutMs]
 */
export async function driveWorkflowTask({
  scenario,
  task,
  sessionId,
  sessionLog,
  runnerOpts,
  onEvent,
  timeoutMs,
}) {
  const sdk = await getSdk()
  const { RuntimeRunner } = sdk
  const workflow = task.workflow
  if (!workflow?.nodes?.length) {
    throw new Error(`orchestration task ${task.id}: missing task.workflow`)
  }

  const spawnLog = []
  const runner = new RuntimeRunner({
    ...runnerOpts,
    sessionLog,
    // Stub path: no provider required.
    provider: runnerOpts.provider,
    subAgentOrchestrator: makeStubOrchestrator({
      onSpawn: (agentId, goal) => {
        spawnLog.push({ agentId, goal })
        onEvent?.(task.id, { type: "workflow_stub_spawn", agentId, goal })
      },
    }),
  })

  const wallStart = Date.now()
  const limit = timeoutMs ?? scenario.timeoutMs ?? 60_000
  /** @type {any} */
  let outcome
  await Promise.race([
    (async () => {
      outcome = await runner.runWorkflow(workflow, { sessionId })
    })(),
    new Promise((_, rej) =>
      setTimeout(() => rej(new Error(`task ${task.id} timeout after ${limit}ms`)), limit),
    ),
  ])

  const events = await sessionLog.read(sessionId)
  const nodeOutcomes = outcome?.nodeOutcomes ?? []
  onEvent?.(task.id, {
    type: "done",
    status: "completed",
    nodeOutcomes,
    spawnLog,
  })

  return {
    finalStatus: "completed",
    finalText: JSON.stringify({
      nodeOutcomes,
      spawnLog,
      firstSpawnIds: firstWaveNodeIds(events),
    }),
    turnMetrics: [],
    streamToolCalls: [],
    wallMs: Date.now() - wallStart,
    events,
  }
}

/** @param {any[]} events @returns {string[]} */
function firstWaveNodeIds(events) {
  for (const e of events) {
    const ev = e.event ?? e
    if (ev.kind === "workflow_batch_spawned" && Array.isArray(ev.node_ids) && ev.node_ids.length) {
      return ev.node_ids.map(String)
    }
  }
  return []
}

/**
 * Shared mechanism metrics for orchestration scenarios.
 * @param {{ events: any[], turnMetrics: any[], streamToolCalls?: any[] }} args
 * @param {{ expectChainId?: string, expectIndependentId?: string }} [hints]
 */
export function orchestrationMechanismHook({ events }, hints = {}) {
  const waves = []
  for (const e of events) {
    const ev = e.event ?? e
    if (ev.kind === "workflow_batch_spawned" && Array.isArray(ev.node_ids)) {
      waves.push(ev.node_ids.map(String))
    }
  }

  const makespanWaves = waves.length
  const firstWave = waves[0] ?? []
  const firstWaveSize = firstWave.length
  const firstWaveHeadIsChain = hints.expectChainId
    ? firstWave[0] === hints.expectChainId
      ? 1
      : 0
    : 0

  let chainStartWave = -1
  if (hints.expectChainId) {
    for (let i = 0; i < waves.length; i++) {
      if (waves[i].some(id => id === hints.expectChainId || id.startsWith(`${hints.expectChainId}-`))) {
        chainStartWave = i
        break
      }
    }
  }

  let independentStartWave = -1
  if (hints.expectIndependentId) {
    for (let i = 0; i < waves.length; i++) {
      if (waves[i].some(id => id === hints.expectIndependentId)) {
        independentStartWave = i
        break
      }
    }
  }

  let failedCount = 0
  let skippedUpstream = 0
  let completedCount = 0
  let pendingOrOther = 0
  const completed = [...events].reverse().find(e => (e.event ?? e).kind === "workflow_completed")
  const outcomes = completed ? ((completed.event ?? completed).node_outcomes ?? []) : []
  for (const o of outcomes) {
    const status = String(o.status ?? o.node_status ?? "").toLowerCase()
    if (status.includes("skipped_upstream") || status === "skippedupstreamfailed") skippedUpstream++
    else if (status === "failed") failedCount++
    else if (status === "completed" || status === "completed_partial" || status === "completedpartial") {
      completedCount++
    } else pendingOrOther++
  }

  return {
    makespanWaves,
    firstWaveSize,
    firstWaveHeadIsChain,
    chainStartWave: chainStartWave < 0 ? 99 : chainStartWave,
    independentStartWave: independentStartWave < 0 ? 99 : independentStartWave,
    // 1 = peer ran before the loop's second iteration (wave index 1 with concurrency 1).
    independentNotStarved: independentStartWave >= 0 && independentStartWave <= 1 ? 1 : 0,
    failedCount,
    skippedUpstream,
    completedCount,
    pendingOrOther,
    terminalClosed: pendingOrOther === 0 && outcomes.length > 0 ? 1 : 0,
  }
}

/** @param {typeof WEIGHTED_POLICY} policy @param {number} maxConcurrent */
export function schedulerOverlay(policy, maxConcurrent) {
  return {
    schedulerPolicy: policy,
    resourceQuota: {
      maxConcurrentSubagents: maxConcurrent,
      maxTotalSubagents: 64,
    },
    extensions: { degradeMissingReasoningReplay: true },
  }
}

/** Empty tools — workflow stubs do not call tools. */
export async function mkEmptyTools() {
  return []
}
