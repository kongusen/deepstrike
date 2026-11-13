// Plan task 9.1 on the real WASM kernel (mirrors node/tests/notes.test.ts): the `note` tool is
// offered only with a notes budget, consumed by the kernel (never executed by the host), shown in
// the task state from the next turn on, and counted in the lifecycle report.
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

function scripted() {
  const run = { offered: [], states: [], calls: 0 }
  run.provider = {
    async complete() { return { role: "assistant", content: "", toolCalls: [] } },
    async *stream(context, tools) {
      run.calls += 1
      run.offered.push(tools.map(t => t.name))
      run.states.push(JSON.stringify(context.stateTurn ?? ""))
      if (run.calls === 1) {
        yield { type: "tool_call", id: "c1", name: "read", arguments: { path: "logs/a.log" } }
        return
      }
      if (run.calls === 2 && tools.some(t => t.name === "note")) {
        yield { type: "tool_call", id: "c2", name: "note", arguments: { title: "Tally", kind: "state", body: "3 qualifying lines in logs/a.log" } }
        return
      }
      yield { type: "text_delta", delta: "3" }
    },
  }
  return run
}

async function events(run, sessionId, notesBudgetTokens) {
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  plane.register(tool("read", "read a file", { type: "object", properties: { path: { type: "string" } } }, () => "ERROR x\nERROR y\nERROR z"))
  const runner = new RuntimeRunner({
    provider: run.provider, sessionLog, executionPlane: plane, maxTokens: 8000, maxTurns: 10,
    baselineToolIds: ["read"], ...(notesBudgetTokens === undefined ? {} : { contextPolicy: { notesBudgetTokens } }),
  })
  assert.equal(await collectText(runner.run({ sessionId, goal: "count the errors" })), "3")
  return (await sessionLog.read(sessionId)).map(entry => entry.event)
}

{
  const run = scripted()
  const log = await events(run, "notes", 600)
  assert.ok(run.offered[0].includes("note"))
  assert.ok(run.states[2].includes("[state] Tally: 3 qualifying lines in logs/a.log"), run.states[2])
  const report = log.filter(event => event.kind === "context_lifecycle_reported").at(-1)
  assert.equal(report.notes_written, 1)
  assert.equal(report.notes_active, 1)
}
{
  // DEC-13 · on by default, off with a zero budget.
  const byDefault = scripted()
  await events(byDefault, "default-notes", undefined)
  assert.ok(byDefault.offered[0].includes("note"))
  const off = scripted()
  await events(off, "no-notes", 0)
  assert.ok(!off.offered[0].includes("note"))
}
console.log("notes: 3 scenarios passed against the real WASM kernel")
