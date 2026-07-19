/**
 * Self-Harness live TaskAdapter (H3) — a real single-attempt run + judge verdict.
 *
 * Where the fixture adapter fakes outcomes, this one actually drives the model: it folds the manifest
 * onto base `RuntimeOptions` with `applyManifest` (instructions → composed system prompt, nudge rules →
 * injectNote channel, whitelisted runtime keys), runs ONE attempt through `RuntimeRunner`, reads the
 * session-log events, and scores the result with the kernel's `judge()`. Its return shape is identical
 * to the fixture adapter's `{ passed, verdict, events, termination }`, so the loop, evidence pipeline,
 * and validation stage are agnostic to which adapter produced a run.
 *
 * Approved deviation: `runTask(task, manifest)` — the adapter (not the loop) constructs base
 * RuntimeOptions and calls `applyManifest`.
 *
 * This adapter needs real provider credentials and cannot be exercised in CI; it is written to mirror
 * `benchmark/core/runner.mjs` conventions (buildJudgeResult / judge) and reviewed by reading, not run.
 *
 * @typedef {import("../../utils/sdk.mjs").ProviderDescriptor} ProviderDescriptor
 * @typedef {import("./fixture.mjs").Task} Task
 * @typedef {import("./fixture.mjs").RunOutcome} RunOutcome
 * @typedef {import("./fixture.mjs").TaskAdapter} TaskAdapter
 */

import { loadSdk } from "../../utils/sdk.mjs"

/**
 * Build a live adapter.
 * @param {{
 *   providerDesc: ProviderDescriptor,
 *   judgeProviderDesc?: ProviderDescriptor,
 *   tasks: Task[],
 *   systemPrompt?: string,
 *   maxTurns?: number,
 *   maxTokens?: number,
 *   mkTools?: (sessionId: string, task: Task) => Promise<any[]> | any[],
 *   timeoutMs?: number,
 * }} config
 * @returns {TaskAdapter}
 */
export function createLiveAdapter(config) {
  const {
    providerDesc,
    judgeProviderDesc = providerDesc,
    tasks,
    systemPrompt = "You are an agent operating under a self-improving harness.",
    maxTurns = 12,
    maxTokens,
    mkTools,
    timeoutMs = 300_000,
  } = config

  if (!Array.isArray(tasks) || tasks.length === 0) {
    throw new Error("createLiveAdapter: config.tasks must be a non-empty Task[]")
  }

  let sdkP
  const getSdk = () => (sdkP ??= loadSdk())

  return {
    id: "live",
    listTasks: () => tasks.map(t => ({ ...t, criteria: t.criteria.map(c => ({ ...c })) })),

    /** @param {Task} task @param {any} manifest @returns {Promise<RunOutcome>} */
    async runTask(task, manifest) {
      const sdk = await getSdk()
      const {
        RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, createProvider, judge, applyManifest,
      } = sdk

      const provider = createProvider({
        provider: providerDesc.provider,
        model: providerDesc.model,
        apiKey: providerDesc.apiKey,
        ...(providerDesc.baseURL ? { baseURL: providerDesc.baseURL } : {}),
        ...(providerDesc.endpoint ? { endpoint: providerDesc.endpoint } : {}),
        retry: { maxRetries: 2, baseDelay: 600 },
      })

      const sessionId = `selfharness-live-${task.id}`
      const sessionLog = new InMemorySessionLog()
      const plane = new LocalExecutionPlane()
      if (typeof mkTools === "function") {
        const tools = (await mkTools(sessionId, task)) ?? []
        for (const t of tools) plane.register(t)
      }

      // Fold the manifest onto base RuntimeOptions — instructions/nudges/runtime keys ride through here.
      const base = {
        systemPrompt,
        maxTurns: task.maxTurns ?? maxTurns,
        ...(maxTokens ? { maxTokens } : {}),
      }
      const runtimeOptions = applyManifest(manifest, base)
      const runner = new RuntimeRunner({ ...runtimeOptions, provider, sessionLog, executionPlane: plane })

      let finalStatus = "error"
      let finalText = ""
      const runPromise = (async () => {
        for await (const evt of runner.run({ sessionId, goal: task.goal, criteria: task.criteria.map(c => c.text) })) {
          if (evt.type === "done") finalStatus = evt.status ?? "error"
          else if (evt.type === "text_delta") finalText += evt.delta ?? ""
        }
      })()
      await Promise.race([
        runPromise,
        new Promise((_, rej) => setTimeout(() => rej(new Error(`task ${task.id} timeout after ${timeoutMs}ms`)), timeoutMs)),
      ])

      const events = await sessionLog.read(sessionId)
      const termination = terminationOf(events, finalStatus)

      const judgeProvider = sameDesc(judgeProviderDesc, providerDesc)
        ? provider
        : createProvider({
          provider: judgeProviderDesc.provider,
          model: judgeProviderDesc.model,
          apiKey: judgeProviderDesc.apiKey,
          ...(judgeProviderDesc.baseURL ? { baseURL: judgeProviderDesc.baseURL } : {}),
          ...(judgeProviderDesc.endpoint ? { endpoint: judgeProviderDesc.endpoint } : {}),
          retry: { maxRetries: 2, baseDelay: 600 },
        })

      const verdict = await judge({
        provider: judgeProvider,
        goal: task.goal,
        criteria: task.criteria.map(c => ({ text: c.text, required: true, ...(c.id ? { id: c.id } : {}) })),
        result: buildJudgeResult({ finalStatus, finalText, events }),
      })

      return { passed: verdict.passed, verdict, events, termination }
    },
  }
}

/** Read run_terminal.reason from the session events; fall back to the stream's final status. */
function terminationOf(events, finalStatus) {
  for (const e of events) {
    const ev = e && typeof e === "object" && "event" in e ? e.event : e
    if (ev && ev.kind === "run_terminal") return String(ev.reason ?? finalStatus)
  }
  return finalStatus
}

function sameDesc(a, b) {
  return a === b || (a && b && a.provider === b.provider && a.model === b.model)
}

/**
 * Build the "agent output" string fed to the judge — final text plus a compact tool-call trail so the
 * judge sees deliverables placed in tool arguments. Mirrors runner.mjs buildJudgeResult.
 */
function buildJudgeResult({ finalStatus, finalText, events }) {
  const text = String(finalText ?? "").trim()
  const trail = []
  for (const e of events) {
    const ev = e && typeof e === "object" && "event" in e ? e.event : e
    if (!ev || ev.kind !== "tool_requested") continue
    for (const c of ev.calls ?? []) {
      let args = typeof c.arguments === "string" ? c.arguments : JSON.stringify(c.arguments ?? {})
      if (args.length > 1500) args = args.slice(0, 1500) + `… [${args.length - 1500} chars truncated]`
      trail.push(`  ${trail.length + 1}. ${c.name ?? "?"}(${args})`)
    }
  }
  const trailBlock = trail.length ? `\n\nTool calls (${trail.length}):\n${trail.join("\n")}` : ""
  if (finalStatus === "completed") return `${text || "(agent produced no text reply)"}${trailBlock}`
  const tail = text ? `\n\nLast assistant text:\n${text}` : ""
  return `AGENT_INCOMPLETE (status=${finalStatus}): ${trail.length} tool calls.${tail}${trailBlock}`
}

/**
 * CLI entry point. Builds a live adapter from resolved provider descriptors and a small built-in demo
 * task set. Real deployments should call `createLiveAdapter` directly with their own tasks.
 * @param {{ providerDesc: ProviderDescriptor, judgeProviderDesc?: ProviderDescriptor }} ctx
 * @returns {TaskAdapter}
 */
export function createAdapter(ctx) {
  return createLiveAdapter({
    providerDesc: ctx.providerDesc,
    judgeProviderDesc: ctx.judgeProviderDesc,
    tasks: DEMO_TASKS,
  })
}

/** A tiny built-in task set so `--adapter live` is runnable end-to-end with real credentials. */
const DEMO_TASKS = [
  {
    id: "explain-recursion",
    goal: "Explain recursion to a beginner in under 120 words with one concrete example.",
    criteria: [
      { id: "R_define", text: "the answer defines recursion correctly" },
      { id: "R_example", text: "the answer includes one concrete example" },
    ],
  },
  {
    id: "summarize-tradeoff",
    goal: "State one concrete tradeoff between depth-first and breadth-first search.",
    criteria: [{ id: "T_tradeoff", text: "the answer states a real, correct tradeoff" }],
  },
]

/** Default seed manifest for the live CLI path. */
export function seedManifest() {
  return {
    manifestVersion: 1,
    parent: null,
    instructions: { execution: "Answer directly and concisely." },
    editableSurfaces: [
      "instructions.bootstrap",
      "instructions.execution",
      "instructions.verification",
      "instructions.failureRecovery",
      "nudges",
      "runtime.maxTurns",
    ],
    audit: { round: 0, createdBy: "seed" },
  }
}
