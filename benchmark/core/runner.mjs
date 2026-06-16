/**
 * runBench — execute one (scenario, variant) and return a MetricSet.
 *
 * Two modes:
 *   - "live"   → uses createProvider(providerDesc) to call a real LLM API.
 *   - "replay" → uses the SDK's ReplayProvider against a fixture of recorded LLM responses.
 *
 * Replay mode reads each task's `<fixtureRoot>/<scenarioId>.<fixtureVariantId>/<taskId>.events.json`
 * (the same files the runner writes on every live run). `fixtureVariantId` defaults to the variant
 * being replayed (sanity mode); when set explicitly (`fixtureFromVariant`), the SAME fixture is
 * used for every replay variant — this is the cross-variant cost-Δ test: model behavior pinned,
 * only the variant's prompt differs.
 *
 * Replay mode emits MetricSet.meta.mode = "replay" so the diff renderer treats any non-zero Δ as
 * significant (it's deterministic — no sample noise).
 *
 * @typedef {import("./scenario.mjs").BenchScenario} BenchScenario
 * @typedef {import("./metrics.mjs").MetricSet} MetricSet
 *
 * @typedef {Object} RunBenchOpts
 * @property {BenchScenario} scenario
 * @property {string} variantId
 * @property {import("../utils/sdk.mjs").ProviderDescriptor} providerDesc
 * @property {string} runRoot                  Per-run output directory.
 * @property {"live" | "replay"} [mode]        Defaults to "live".
 * @property {string} [fixtureRoot]            Required when mode="replay". Path to a prior runRoot.
 * @property {string} [fixtureFromVariant]     When set, replay reads from this variant's events.json
 *                                             instead of the variant being run (cross-variant pin).
 * @property {Record<string, any>} [pricing]
 * @property {number} [maxTasks]               Limit to first N tasks (default: all).
 * @property {(taskId: string, evt: any) => void} [onEvent]  Stream tap for CLI logging.
 *
 * @typedef {Object} RunBenchResult
 * @property {MetricSet} metricSet
 * @property {string} metricSetPath            Path written.
 * @property {Array<{ taskId: string, sessionId: string, status: string, error?: string }>} sessions
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import path from "node:path"

import { loadSdk } from "../utils/sdk.mjs"
import { buildMetricSet } from "./aggregator.mjs"

/** @param {RunBenchOpts} opts @returns {Promise<RunBenchResult>} */
export async function runBench(opts) {
  const { scenario, variantId, providerDesc, runRoot, pricing, maxTasks, onEvent } = opts
  const mode = opts.mode ?? "live"
  const variant = scenario.variants[variantId]
  if (!variant) {
    throw new Error(`scenario ${scenario.id}: unknown variant "${variantId}". Known: ${Object.keys(scenario.variants).join(", ")}`)
  }
  if (mode === "replay" && !opts.fixtureRoot) {
    throw new Error(`runBench: mode="replay" requires opts.fixtureRoot`)
  }

  const sdk = await loadSdk()
  const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, createProvider, ReplayProvider, extractRecordedMessages } = sdk
  if (mode === "replay" && (!ReplayProvider || !extractRecordedMessages)) {
    throw new Error(`runBench: SDK does not export ReplayProvider / extractRecordedMessages — rebuild node SDK (mode=replay)`)
  }

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
    const provider = mode === "live"
      ? createProvider({
        provider: providerDesc.provider,
        model: providerDesc.model,
        apiKey: providerDesc.apiKey,
        ...(providerDesc.baseURL ? { baseURL: providerDesc.baseURL } : {}),
        ...(providerDesc.endpoint ? { endpoint: providerDesc.endpoint } : {}),
        retry: { maxRetries: 2, baseDelay: 600 },
      })
      : buildReplayProvider({
        fixtureRoot: opts.fixtureRoot,
        scenarioId: scenario.id,
        fixtureVariantId: opts.fixtureFromVariant ?? variantId,
        taskId: task.id,
        ReplayProvider,
        extractRecordedMessages,
      })

    const runnerOpts = {
      provider,
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

  const fixtureNote = mode === "replay"
    ? ` · fixture=${path.basename(opts.fixtureRoot ?? "")}/${opts.fixtureFromVariant ?? variantId}`
    : ""
  const metricSet = buildMetricSet({
    scenarioId: scenario.id,
    variantId,
    provider: providerDesc.provider,
    model: providerDesc.model,
    mode,
    sessions: sessionRecords,
    pricing,
    mechanismHook: scenario.mechanismHook,
    notes: `runBench ${mode} · ${tasks.length} tasks · variant ${variant.description}${fixtureNote}`,
  })

  const metricSetPath = path.join(variantDir, "metricset.json")
  writeFileSync(metricSetPath, JSON.stringify(metricSet, null, 2))

  return { metricSet, metricSetPath, sessions: sessionStatuses }
}

/**
 * Build a SDK ReplayProvider for one task by reading the fixture's events.json.
 * @param {{
 *   fixtureRoot: string,
 *   scenarioId: string,
 *   fixtureVariantId: string,
 *   taskId: string,
 *   ReplayProvider: any,
 *   extractRecordedMessages: any,
 * }} args
 */
function buildReplayProvider(args) {
  const eventsPath = path.join(
    args.fixtureRoot,
    `${args.scenarioId}.${args.fixtureVariantId}`,
    `${args.taskId}.events.json`,
  )
  if (!existsSync(eventsPath)) {
    throw new Error(
      `replay fixture missing: ${eventsPath}. ` +
      `Record a live run first: bench ${args.scenarioId} --variants ${args.fixtureVariantId} --provider <id>`,
    )
  }
  const events = JSON.parse(readFileSync(eventsPath, "utf8"))
  const messages = args.extractRecordedMessages(events)
  if (messages.length === 0) {
    throw new Error(`replay fixture has no llm_completed events: ${eventsPath}`)
  }
  return new args.ReplayProvider(messages)
}
