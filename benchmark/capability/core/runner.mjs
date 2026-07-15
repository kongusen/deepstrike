/**
 * runCapability — drive one CapAdapter over N tasks via RuntimeRunner.
 */

import { mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"

import { loadSdk } from "../../utils/sdk.mjs"
import { buildReport, writeReport } from "./report.mjs"

/**
 * @param {{
 *   adapter: import("./types.mjs").CapAdapter,
 *   providerDesc: import("../../utils/sdk.mjs").ProviderDescriptor,
 *   runRoot: string,
 *   limit?: number,
 *   dataset?: string,
 *   onEvent?: (taskId: string, evt: any) => void,
 * }} opts
 */
export async function runCapability(opts) {
  const { adapter, providerDesc, runRoot } = opts
  const startedAt = new Date().toISOString()
  mkdirSync(runRoot, { recursive: true })

  const sdk = await loadSdk()
  const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, createProvider } = sdk
  if (!RuntimeRunner || !LocalExecutionPlane || !createProvider) {
    throw new Error("runCapability: Node SDK missing RuntimeRunner / LocalExecutionPlane / createProvider — rebuild node/")
  }

  const tasks = await adapter.loadTasks({ limit: opts.limit, dataset: opts.dataset })
  if (!tasks.length) {
    throw new Error(`capability ${adapter.id}: no tasks loaded`)
  }

  /** @type {import("./types.mjs").CapResult[]} */
  const results = []
  const maxTurns = adapter.maxTurns ?? 12
  const maxTokens = adapter.maxTokens ?? 4096
  const timeoutMs = adapter.timeoutMs ?? 180_000
  const systemPrompt = adapter.systemPrompt ?? defaultSystemPrompt(adapter.id)

  for (const task of tasks) {
    const sessionId = `cap-${adapter.id}-${task.id}-${Date.now()}`
    const sessionLog = new InMemorySessionLog()
    const plane = new LocalExecutionPlane()
    const tools = await adapter.mkTools(task, sdk)
    for (const t of tools) plane.register(t)

    const provider = createProvider({
      provider: providerDesc.provider,
      model: providerDesc.model,
      apiKey: providerDesc.apiKey,
      ...(providerDesc.baseURL ? { baseURL: providerDesc.baseURL } : {}),
      ...(providerDesc.endpoint ? { endpoint: providerDesc.endpoint } : {}),
      retry: { maxRetries: 2, baseDelay: 600 },
    })

    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: plane,
      maxTokens,
      maxTurns,
      systemPrompt,
    })

    let finalStatus = "error"
    let errorMsg
    let finalText = ""
    /** @type {import("./types.mjs").CapToolCall[]} */
    const toolCalls = []
    const wallStart = Date.now()

    try {
      const runPromise = (async () => {
        for await (const evt of runner.run({ sessionId, goal: task.goal })) {
          if (evt.type === "done") finalStatus = evt.status ?? "error"
          else if (evt.type === "text_delta") finalText += evt.delta ?? ""
          else if (evt.type === "tool_call") {
            toolCalls.push({ name: evt.name, arguments: evt.arguments ?? {} })
          }
          opts.onEvent?.(task.id, evt)
        }
      })()
      await Promise.race([
        runPromise,
        new Promise((_, rej) =>
          setTimeout(() => rej(new Error(`task ${task.id} timeout after ${timeoutMs}ms`)), timeoutMs),
        ),
      ])
    } catch (e) {
      errorMsg = e?.message ? String(e.message) : String(e)
      if (errorMsg.includes("timeout")) finalStatus = "timeout"
      else if (finalStatus === "error") finalStatus = "exception"
    }

    const wallMs = Date.now() - wallStart
    let grade
    try {
      grade = await adapter.grade({
        task,
        finalText,
        toolCalls,
        status: finalStatus,
      })
    } catch (e) {
      grade = {
        passed: false,
        score: 0,
        reason: `grader error: ${e?.message ? String(e.message) : String(e)}`,
      }
    }

    /** @type {import("./types.mjs").CapResult} */
    const result = {
      taskId: task.id,
      sessionId,
      status: finalStatus,
      finalText,
      toolCalls,
      wallMs,
      grade,
      ...(errorMsg ? { error: errorMsg } : {}),
    }
    results.push(result)

    try {
      writeFileSync(
        path.join(runRoot, `${task.id}.result.json`),
        JSON.stringify(result, null, 2),
      )
      const events = await sessionLog.read(sessionId)
      writeFileSync(
        path.join(runRoot, `${task.id}.events.json`),
        JSON.stringify(events, null, 2),
      )
    } catch { /* best-effort */ }

    opts.onEvent?.(task.id, {
      type: "capability_grade",
      passed: grade.passed,
      score: grade.score,
      reason: grade.reason,
    })
  }

  const finishedAt = new Date().toISOString()
  const report = buildReport({
    suite: adapter.id,
    provider: providerDesc.provider,
    model: providerDesc.model,
    startedAt,
    finishedAt,
    results,
    notes: `capability ${adapter.id} · ${tasks.length} tasks · maxTurns=${maxTurns}`,
  })
  const reportPath = writeReport(report, runRoot)
  return { report, reportPath, results }
}

/** @param {string} suiteId */
function defaultSystemPrompt(suiteId) {
  if (suiteId === "bfcl") {
    return [
      "You are a function-calling agent.",
      "Use the provided tools to fulfill the user request.",
      "Call the correct tool(s) with exact argument names and values.",
      "Prefer tool calls over free-form answers when a tool can satisfy the request.",
      "When finished, give a short plain-text confirmation.",
    ].join("\n")
  }
  if (suiteId === "gaia") {
    return [
      "You are a careful research agent.",
      "Use tools to gather evidence before answering.",
      "When you know the final answer, reply with ONLY the answer on the last line",
      "prefixed by 'Final answer: ' (no extra commentary after it).",
    ].join("\n")
  }
  return "You are a helpful agent. Use tools when needed and finish with a clear answer."
}
