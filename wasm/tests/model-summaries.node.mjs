// Plan task 8.4 on the real WASM kernel (mirrors node/tests/model-summaries.test.ts): a range
// compaction asks the runner's model for its summary through `summarize_context`, the reply replaces
// the rule summary before the next provider call, and a failed call keeps the rule summary.
import assert from "node:assert/strict"
import { register } from "node:module"
import { pathToFileURL } from "node:url"
const kernelUrl = new URL("../pkg-node/deepstrike_wasm.js", import.meta.url).href
register(`data:text/javascript,${encodeURIComponent(`export async function resolve(s,c,n) {
  if (s === "@deepstrike/wasm-kernel") return {url:${JSON.stringify(kernelUrl)},shortCircuit:true}; return n(s,c)
}`)}`, pathToFileURL("./"))
const { RuntimeRunner, collectText } = await import("../dist/runtime/runner.js")
const { InMemorySessionLog } = await import("../dist/runtime/session-log.js")
const { LocalExecutionPlane } = await import("../dist/runtime/execution-plane.js")
const { tool } = await import("../dist/tools/index.js")

const SUMMARY = "MODEL SUMMARY: filled rows 1-6, 6 rows so far"

function scripted(failSummary) {
  const run = { states: [], summaryCalls: 0, calls: 0 }
  run.provider = {
    async complete() { return { role: "assistant", content: "", toolCalls: [] } },
    async *stream(context) {
      if ((context.systemStable ?? "").startsWith("You compress")) {
        run.summaryCalls += 1
        if (failSummary) {
          yield { type: "error", message: "model unreachable" }
          return
        }
        yield { type: "text_delta", delta: SUMMARY }
        yield { type: "usage", totalTokens: 120, inputTokens: 100, outputTokens: 20 }
        return
      }
      run.calls += 1
      run.states.push(JSON.stringify(context.stateTurn ?? ""))
      if (run.calls <= 12) {
        yield { type: "text_delta", delta: `row ${run.calls} filled; ${run.calls} rows so far. ` }
        yield { type: "tool_call", id: `c${run.calls}`, name: "fill", arguments: { n: run.calls } }
        return
      }
      yield { type: "text_delta", delta: "done" }
    },
  }
  return run
}

async function events(run, sessionId, modelSummaries) {
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  plane.register(tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () => "w".repeat(600)))
  const runner = new RuntimeRunner({
    provider: run.provider, sessionLog, executionPlane: plane, maxTokens: 1200, maxTurns: 30,
    baselineToolIds: ["fill"], ...(modelSummaries ? { modelSummaries } : {}),
  })
  await collectText(runner.run({ sessionId, goal: "fill rows" }))
  return (await sessionLog.read(sessionId)).map(entry => entry.event)
}

{
  const run = scripted(false)
  const log = await events(run, "model-summary", true)
  const summarized = log.filter(event => event.kind === "context_summarized")
  assert.ok(run.summaryCalls > 0, "a range compaction asked for a summary")
  assert.equal(summarized.length, run.summaryCalls)
  assert.equal(summarized[0].status, "success")
  assert.equal(summarized[0].summary, SUMMARY)
  assert.equal(summarized[0].usage.inputTokens, 100)
  assert.ok(run.states.some(state => state.includes("model summary") && state.includes(SUMMARY)))
  assert.equal(log.find(event => event.kind === "context_lifecycle_reported").model_summaries, run.summaryCalls)
}
{
  const run = scripted(true)
  const log = await events(run, "model-summary-failed", true)
  const failed = log.find(event => event.kind === "context_summarized")
  assert.ok(run.summaryCalls > 0)
  assert.equal(failed.status, "failed")
  assert.match(failed.error, /model unreachable/)
  assert.ok(run.states.some(state => state.includes("[Compressed:")))
  assert.ok(!run.states.some(state => state.includes("model summary")))
}
{
  const run = scripted(false)
  const log = await events(run, "model-summary-off")
  assert.ok(log.some(event => event.kind === "compressed"))
  assert.equal(run.summaryCalls, 0)
}
console.log("model summaries on the real WASM kernel: ok")
