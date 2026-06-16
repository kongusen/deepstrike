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
 * @property {number} [samples]                BM1.2: repeat the full task list N times per variant
 *                                             so stdev tightens. Each sample is a fresh session per
 *                                             task; the aggregator pools across sessions×samples.
 *                                             Default 1.
 * @property {(taskId: string, evt: any) => void} [onEvent]  Stream tap for CLI logging.
 * @property {Object} [judge]                  When set, judge each session's output via SDK.judge().
 * @property {import("../utils/sdk.mjs").ProviderDescriptor} judge.providerDesc
 *                                             Provider for the eval LLM call (often a cheaper model).
 *
 * @typedef {Object} RunBenchResult
 * @property {MetricSet} metricSet
 * @property {string} metricSetPath            Path written.
 * @property {Array<{ taskId: string, sessionId: string, status: string, error?: string,
 *                    passed?: boolean, overallScore?: number }>} sessions
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
  const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, createProvider, ReplayProvider, extractRecordedMessages, judge } = sdk
  if (mode === "replay" && (!ReplayProvider || !extractRecordedMessages)) {
    throw new Error(`runBench: SDK does not export ReplayProvider / extractRecordedMessages — rebuild node SDK (mode=replay)`)
  }
  if (opts.judge && !judge) {
    throw new Error(`runBench: SDK does not export judge() — rebuild node SDK (judge enabled)`)
  }
  // Construct the judge provider once (one provider per variant — sessions share it).
  // Wrap it in a usage-capture so the judge's own LLM cost can be reported separately from the
  // main run cost. Without this, --judge silently adds ~5-10% to the headline $ per A/B.
  const judgeUsage = { inputTokens: 0, outputTokens: 0, cacheReadInputTokens: 0, cacheCreationInputTokens: 0 }
  const judgeProvider = opts.judge
    ? wrapUsageCapture(
      createProvider({
        provider: opts.judge.providerDesc.provider,
        model: opts.judge.providerDesc.model,
        apiKey: opts.judge.providerDesc.apiKey,
        ...(opts.judge.providerDesc.baseURL ? { baseURL: opts.judge.providerDesc.baseURL } : {}),
        ...(opts.judge.providerDesc.endpoint ? { endpoint: opts.judge.providerDesc.endpoint } : {}),
        retry: { maxRetries: 2, baseDelay: 600 },
      }),
      judgeUsage,
    )
    : undefined

  const variantDir = path.join(runRoot, `${scenario.id}.${variantId}`)
  mkdirSync(variantDir, { recursive: true })

  const setup = await variant.setup(scenario, {
    variantId,
    runRoot: variantDir,
    scenarioId: scenario.id,
  })
  const overlay = setup.runtimeOverlay ?? {}

  const tasks = (maxTasks ? scenario.tasks.slice(0, maxTasks) : scenario.tasks)
  const samples = Math.max(1, Math.floor(opts.samples ?? 1))
  const sessionRecords = []
  /** @type {Array<{ taskId: string, sessionId: string, status: string, error?: string }>} */
  const sessionStatuses = []

  // BM1.2: outer loop over samples. Each sample reruns the full task list so the aggregator pools
  // (sample × task) sessions and stdev is computed across the whole pool. samples=1 (default) is
  // exactly the prior behavior — one session per task.
  for (let sampleIdx = 0; sampleIdx < samples; sampleIdx++)
  for (const task of tasks) {
    const sampleSuffix = samples > 1 ? `-s${sampleIdx + 1}` : ""
    const sessionId = `bench-${scenario.id}-${variantId}-${task.id}${sampleSuffix}-${Date.now()}`
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
    let finalText = ""
    /** @type {Array<{ name: string, arguments: Record<string, unknown> }>} */
    const streamToolCalls = []
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
          else if (evt.type === "text_delta") finalText += evt.delta ?? ""
          else if (evt.type === "tool_call") {
            // BM5 #29: model-attempted calls (governance may intercept before tool_requested
            // lands in session log, so this is the only way to count attempts vs executed).
            streamToolCalls.push({ name: evt.name, arguments: evt.arguments ?? {} })
          }
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

    // BM3: judge the session's output if a judge provider was set up and the task has criteria.
    let passed
    let overallScore
    let judgeError
    if (judgeProvider && task.criteria?.length) {
      try {
        const result = buildJudgeResult({ finalStatus, finalText, turnCount: turnMetrics.length, events })
        const verdict = await judge({
          provider: judgeProvider,
          goal: task.goal,
          criteria: task.criteria.map(c => ({ text: c, required: true })),
          result,
        })
        passed = verdict.passed
        overallScore = verdict.overallScore
        onEvent?.(task.id, { type: "judge", passed, overallScore, feedback: verdict.feedback })
      } catch (e) {
        judgeError = e?.message ? String(e.message) : String(e)
        onEvent?.(task.id, { type: "judge_error", message: judgeError })
      }
    }

    sessionRecords.push({
      sessionId,
      taskId: task.id,
      turnMetrics,
      events,
      streamToolCalls,
      wallMs,
      finalStatus,
      finalText,
      passed,
      overallScore,
    })
    sessionStatuses.push({
      taskId: task.id,
      sessionId,
      status: finalStatus,
      ...(errorMsg ? { error: errorMsg } : {}),
      ...(passed !== undefined ? { passed } : {}),
      ...(overallScore !== undefined ? { overallScore } : {}),
      ...(judgeError ? { judgeError } : {}),
    })

    // Persist raw events for debugging / post-hoc replay. With samples>1 each sample writes its
    // own file so they don't clobber; samples=1 keeps the legacy `<task>.events.json` filename
    // (which replay fixtures still read by default).
    try {
      const eventsFilename = samples > 1
        ? `${task.id}${sampleSuffix}.events.json`
        : `${task.id}.events.json`
      writeFileSync(
        path.join(variantDir, eventsFilename),
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
    ...(opts.judge ? {
      judgeUsage,
      judgeProvider: opts.judge.providerDesc.provider,
      judgeModel: opts.judge.providerDesc.model,
    } : {}),
  })

  const metricSetPath = path.join(variantDir, "metricset.json")
  writeFileSync(metricSetPath, JSON.stringify(metricSet, null, 2))

  return { metricSet, metricSetPath, sessions: sessionStatuses }
}

/**
 * Wrap an LLMProvider so every `usage` event seen on its stream() output is also accumulated
 * into the supplied `sink` totals. Pure pass-through for everything else.
 *
 * Used to track judge cost separately from the main run cost (#judge-cost backlog).
 *
 * @param {any} inner
 * @param {{ inputTokens: number, outputTokens: number, cacheReadInputTokens: number, cacheCreationInputTokens: number }} sink
 */
function wrapUsageCapture(inner, sink) {
  return {
    async complete(...args) { return inner.complete(...args) },
    async *stream(...args) {
      for await (const evt of inner.stream(...args)) {
        if (evt.type === "usage") {
          sink.inputTokens += Number(evt.inputTokens) || 0
          sink.outputTokens += Number(evt.outputTokens) || 0
          sink.cacheReadInputTokens += Number(evt.cacheReadInputTokens) || 0
          sink.cacheCreationInputTokens += Number(evt.cacheCreationInputTokens) || 0
        }
        yield evt
      }
    },
    descriptor: inner.descriptor?.bind(inner),
    runtimePolicy: inner.runtimePolicy?.bind(inner),
    peekProviderReplay: inner.peekProviderReplay?.bind(inner),
    seedProviderReplay: inner.seedProviderReplay?.bind(inner),
  }
}

/**
 * Build the "agent output" string fed to the judge. Bakes in run status so the judge can grade
 * incomplete runs honestly — for max_turns / error / exception runs we still get a quality signal
 * rather than a false-clean pass on an empty reply.
 *
 * Includes a structured trail of every tool call the agent issued (name + truncated args). Many
 * scenarios put the agent's actual deliverable into a tool-call argument (`summarize_findings(summary)`,
 * `write_file(content)`, `submit_answer(answer)`) — without this trail the judge only sees the
 * `text_delta` chatter and misses the actual work product, which is what backlog #24 was about.
 *
 * Arg truncation cap (`ARG_CAP`) is per-call, not per-result: a single deliverable up to ~1500 chars
 * survives intact; longer ones get a tail marker. Keeps judge prompts bounded on long loops.
 *
 * @param {{ finalStatus: string, finalText: string, turnCount: number, events: any[] }} args
 */
function buildJudgeResult({ finalStatus, finalText, turnCount, events }) {
  const text = finalText.trim()
  const toolCalls = extractToolCallTrail(events)
  const trailBlock = toolCalls.length === 0
    ? ""
    : `\n\nTool calls (${toolCalls.length}):\n${toolCalls.map((c, i) => `  ${i + 1}. ${c.name}(${c.args})`).join("\n")}`

  if (finalStatus === "completed") {
    const body = text || "(agent produced no text reply)"
    return `${body}${trailBlock}`
  }
  const tail = text ? `\n\nLast assistant text:\n${text}` : ""
  return `AGENT_INCOMPLETE (status=${finalStatus}): ran ${turnCount} LLM turns, ${toolCalls.length} tool calls.${tail}${trailBlock}`
}

const ARG_CAP = 1500

function extractToolCallTrail(events) {
  /** @type {Array<{ name: string, args: string }>} */
  const out = []
  for (const e of events) {
    if (e.event?.kind !== "tool_requested") continue
    for (const c of e.event.calls ?? []) {
      const name = c.name ?? "?"
      let args = typeof c.arguments === "string" ? c.arguments : JSON.stringify(c.arguments ?? {})
      if (args.length > ARG_CAP) args = args.slice(0, ARG_CAP) + `… [${args.length - ARG_CAP} chars truncated]`
      out.push({ name, args })
    }
  }
  return out
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
