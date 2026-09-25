import { count, metric } from "../core/metrics.mjs"
import { collectAsync } from "../core/runtime.mjs"

const scope = { tenant_id: "benchmark-0.2.74", namespace: "agent-facade" }

export const agentFacade = {
  id: "agent-facade",
  description: "root createAgent run/stream/session/evidence and memory boundary",
  variants: ["replay"],
  surfaces: ["root", "os", "advanced", "memory"],
  async run({ sdk }) {
    const provider = new sdk.os.ReplayProvider([{ role: "assistant", content: "hello from replay" }], { wrap: true })
    const sessionLog = new sdk.advanced.InMemorySessionLog()
    const plane = new sdk.advanced.LocalExecutionPlane()
    const agent = sdk.root.createAgent({
      name: "benchmark-agent",
      instructions: "Return the replay fixture exactly.",
      runtimeBinding: { provider, executionPlane: plane, sessionLog },
    })

    const run = await agent.run("say hello", { session: { id: "agent-run" } })
    provider.reset()
    const streamEvents = await collectAsync(agent.stream("say hello", { session: { id: "agent-stream" } }))
    const sessionResult = await agent.session("agent-session").run("say hello")

    const memoryStore = new sdk.memory.InMemoryMemoryStore()
    const memoryAgent = sdk.root.createAgent({ memoryStore, memoryScope: scope })
    const saved = await memoryAgent.remember({ name: "preference", content: "Use focused tests", kind: "project" })
    const recalled = await memoryAgent.recall("focused tests")

    if (run.status !== "completed" || run.output !== "hello from replay") throw new Error("agent run contract returned an unexpected result")
    if (sessionResult.sessionId !== "agent-session") throw new Error("session() did not preserve the requested session id")
    if (!run.evidence?.route || !run.evidence?.measurement || !run.evidence?.contextBinding) throw new Error("run evidence is incomplete")
    if (!streamEvents.some(event => event.type === "text_delta") || !streamEvents.some(event => event.type === "done")) throw new Error("stream contract omitted text_delta or done")
    if (recalled[0]?.record.record_id !== saved.record_id) throw new Error("memory remember/recall boundary did not round-trip")

    return {
      metrics: {
        completedRuns: count(2),
        streamEvents: count(streamEvents.length),
        replayInputTokens: metric(run.usage?.inputTokens ?? run.evidence.measurement.inputTokens ?? 0, "tokens"),
        memoryHits: count(recalled.length),
        evidenceFields: count(Object.keys(run.evidence).length),
      },
      evidence: {
        result: { status: run.status, output: run.output, sessionId: run.sessionId },
        stream: streamEvents.map(event => event.type),
        memory: { recordId: saved.record_id, score: recalled[0]?.score ?? 0 },
      },
    }
  },
}
