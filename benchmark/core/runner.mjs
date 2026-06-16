/**
 * runBench — execute one (scenario, variant) under live mode and return a MetricSet.
 *
 * Live-only for now. Replay mode arrives in a follow-up PR (the SDK doesn't yet ship a
 * request-skipping provider wrapper; `MetricSet.meta.mode` already supports `"replay"`).
 *
 * @typedef {import("./scenario.mjs").BenchScenario} BenchScenario
 * @typedef {import("./metrics.mjs").MetricSet} MetricSet
 *
 * @typedef {Object} RunBenchOpts
 * @property {BenchScenario} scenario
 * @property {string} variantId
 * @property {import("../utils/sdk.mjs").ProviderDescriptor} providerDesc
 * @property {string} runRoot                  Per-run output directory.
 * @property {Record<string, any>} [pricing]
 * @property {number} [maxTasks]               Limit to first N tasks (default: all).
 * @property {(taskId: string, evt: any) => void} [onEvent]  Stream tap for CLI logging.
 *
 * @typedef {Object} RunBenchResult
 * @property {MetricSet} metricSet
 * @property {string} metricSetPath            Path written.
 * @property {Array<{ taskId: string, sessionId: string, status: string, error?: string }>} sessions
 */

import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"

import { loadSdk } from "../utils/sdk.mjs"
import { buildMetricSet } from "./aggregator.mjs"

/** @param {RunBenchOpts} opts @returns {Promise<RunBenchResult>} */
export async function runBench(opts) {
  const { scenario, variantId, providerDesc, runRoot, pricing, maxTasks, onEvent } = opts
  const variant = scenario.variants[variantId]
  if (!variant) {
    throw new Error(`scenario ${scenario.id}: unknown variant "${variantId}". Known: ${Object.keys(scenario.variants).join(", ")}`)
  }

  const sdk = await loadSdk()
  const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, createProvider } = sdk

  const variantDir = path.join(runRoot, `${scenario.id}.${variantId}`)
  mkdirSync(variantDir, { recursive: true })

  const setup = await variant.setup(scenario, {
    variantId,
    runRoot: variantDir,
    scenarioId: scenario.id,
  })
  const overlay = setup.runtimeOverlay ?? {}

  const tasks = (maxTasks ? scenario.tasks.slice(0, maxTasks) : scenario.tasks)
  const sessionRecords = []
  /** @type {Array<{ taskId: string, sessionId: string, status: string, error?: string }>} */
  const sessionStatuses = []

  for (const task of tasks) {
    const sessionId = `bench-${scenario.id}-${variantId}-${task.id}-${Date.now()}`
    const sessionLog = new InMemorySessionLog()
    const plane = new LocalExecutionPlane()
    const tools = await scenario.mkTools(sessionId)
    for (const t of tools) plane.register(t)

    const turnMetrics = []
    const runnerOpts = {
      provider: createProvider({
        provider: providerDesc.provider,
        model: providerDesc.model,
        apiKey: providerDesc.apiKey,
        ...(providerDesc.baseURL ? { baseURL: providerDesc.baseURL } : {}),
        ...(providerDesc.endpoint ? { endpoint: providerDesc.endpoint } : {}),
        retry: { maxRetries: 2, baseDelay: 600 },
      }),
      sessionLog,
      executionPlane: plane,
      maxTokens: scenario.maxTokens,
      maxTurns: scenario.maxTurns,
      systemPrompt: scenario.systemPrompt,
      onTurnMetrics: m => turnMetrics.push({ ...m }),
      ...overlay,
    }

    const runner = new RuntimeRunner(runnerOpts)

    let finalStatus = "error"
    let errorMsg
    let wallStart = Date.now()
    const timeoutMs = scenario.timeoutMs ?? 300_000

    try {
      const runPromise = (async () => {
        for await (const evt of runner.run({
          sessionId,
          goal: task.goal,
          criteria: task.criteria,
        })) {
          if (evt.type === "done") finalStatus = evt.status ?? "error"
          onEvent?.(task.id, evt)
        }
      })()
      await Promise.race([
        runPromise,
        new Promise((_, rej) => setTimeout(() => rej(new Error(`task ${task.id} timeout after ${timeoutMs}ms`)), timeoutMs)),
      ])
    } catch (e) {
      errorMsg = e?.message ? String(e.message) : String(e)
      finalStatus = finalStatus === "error" ? "exception" : finalStatus
    }

    const wallMs = Date.now() - wallStart
    const events = await sessionLog.read(sessionId)
    sessionRecords.push({
      sessionId,
      taskId: task.id,
      turnMetrics,
      events,
      wallMs,
      finalStatus,
      // BM3 (judge) will set passed; for now leave undefined → quality.successRate omitted.
      passed: undefined,
    })
    sessionStatuses.push({ taskId: task.id, sessionId, status: finalStatus, ...(errorMsg ? { error: errorMsg } : {}) })

    // Persist raw events for debugging / post-hoc replay
    try {
      writeFileSync(
        path.join(variantDir, `${task.id}.events.json`),
        JSON.stringify(events, null, 2),
      )
    } catch { /* best-effort */ }
  }

  try { await setup.cleanup?.() } catch { /* swallow */ }

  if (sessionRecords.length === 0) {
    throw new Error(`scenario ${scenario.id} variant ${variantId}: no sessions produced (maxTasks=${maxTasks})`)
  }

  const metricSet = buildMetricSet({
    scenarioId: scenario.id,
    variantId,
    provider: providerDesc.provider,
    model: providerDesc.model,
    mode: "live",
    sessions: sessionRecords,
    pricing,
    mechanismHook: scenario.mechanismHook,
    notes: `runBench live · ${tasks.length} tasks · variant ${variant.description}`,
  })

  const metricSetPath = path.join(variantDir, "metricset.json")
  writeFileSync(metricSetPath, JSON.stringify(metricSet, null, 2))

  return { metricSet, metricSetPath, sessions: sessionStatuses }
}
