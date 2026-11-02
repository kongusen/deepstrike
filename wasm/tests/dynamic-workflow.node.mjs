// Plan task 6.3 · WASM SDK × real kernel (pkg-node): a host program runs agents on a dynamic
// workflow root and shares state with them through the blackboard (plan task 6.2).
import assert from "node:assert/strict"
import { register } from "node:module"

const kernelUrl = new URL("../pkg-node/deepstrike_wasm.js", import.meta.url).href
register(`data:text/javascript,${encodeURIComponent(`export async function resolve(s,c,n) {
  if (s === "@deepstrike/wasm-kernel") return {url:${JSON.stringify(kernelUrl)},shortCircuit:true}; return n(s,c)
}`)}`)

const dist = new URL("../dist/", import.meta.url)
const { RuntimeRunner } = await import(new URL("runtime/runner.js", dist).href)
const { InMemorySessionLog } = await import(new URL("runtime/session-log.js", dist).href)
const { dynamicAgentTask, DynamicWorkflowLimitError } = await import(new URL("runtime/dynamic-workflow.js", dist).href)

const seen = {}
const runner = () => new RuntimeRunner({
  sessionLog: new InMemorySessionLog(), maxTokens: 8000,
  provider: { async complete() { return { role: "assistant", content: "", toolCalls: [] } }, async *stream() {} },
  subAgentOrchestrator: {
    async run(ctx) {
      const goal = ctx.spec.goal.split("\n")[0]
      seen[goal] = (ctx.inheritedKnowledge ?? []).map(entry => `${entry.key}=${entry.content}`)
      return { agentId: ctx.manifest.agent_id, result: { termination: "completed", finalMessage: { role: "assistant", content: `done: ${goal}`, toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 } }
    },
  },
})

const run = await runner().runDynamicWorkflow(async ctx => {
  const first = await ctx.agent("survey the field")
  await ctx.blackboard.put("criteria", "primary sources only")
  const pair = await ctx.phase("draft", async () => {
    await ctx.blackboard.put("outline", { sections: 3 }, { scope: "phase" })
    return ctx.parallelAgents(["one", "two"], part => dynamicAgentTask(`draft part ${part}`))
  })
  const last = await ctx.agent("review", { label: "reviewer" })
  return [first.text, ...pair.map(result => result.text), last.nodeId, ctx.blackboard.get("outline")]
}, { sessionId: "dw" })

assert.deepEqual(run.value, ["done: survey the field", "done: draft part one", "done: draft part two", "reviewer", undefined])
assert.deepEqual(seen["survey the field"], [])
assert.deepEqual(seen["draft part one"], ["blackboard:criteria=primary sources only", 'blackboard:outline={"sections":3}'])
assert.deepEqual(seen["review"], ["blackboard:criteria=primary sources only"])
assert.equal(run.progress.agentsCompleted, 4)

await assert.rejects(
  runner().runDynamicWorkflow(async ctx => { await ctx.agent("one"); await ctx.agent("two") }, { limits: { maxAgentsPerRun: 1 } }),
  DynamicWorkflowLimitError,
)
console.log("dynamic workflow: real WASM kernel program, blackboard and limits pass")
