/** Dynamic-workflow optimization batch: node-observable kernel behavior and per-node caps. */
import { workflowNodeSpecToKernel, workflowNodeToSpec } from "../src/types/agent.js"
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { createRunner, tool } from "./runtime/helpers.js"
import { ReactiveSession } from "../src/runtime/reactive-session.js"
import { InMemoryGroupBudgetStore } from "../src/runtime/run-group.js"
import type { LLMProvider, ModelMessage, StreamEvent } from "../src/types.js"

describe("W-N2 / W-N7: spawn descriptors carry data edges and per-node caps", () => {
  it("workflowNodeSpecToKernel emits max_turns/max_wall_ms and workflowNodeToSpec maps them back", () => {
    const kernelJson = workflowNodeSpecToKernel({
      task: "expensive", role: "implement", tokenBudget: 5000, maxTurns: 4, maxWallMs: 30_000,
    })
    expect(kernelJson.max_turns).toBe(4)
    expect(kernelJson.max_wall_ms).toBe(30_000)

    const spec = workflowNodeToSpec(
      {
        agent_id: "wf-node0", goal: "g", role: "implement", isolation: "shared",
        context_inheritance: "none", token_budget: 5000, max_turns: 4, max_wall_ms: 30_000,
      },
      "parent",
    )
    expect(spec.maxTurns).toBe(4)
    expect(spec.maxWallMs).toBe(30_000)
    expect(spec.tokenBudget).toBe(5000)
  })

  it("carries dynamic per-node caps through the canonical kernel to the child runner", async () => {
    const seen: Array<{ tokenBudget?: number; maxTurns?: number; maxWallMs?: number }> = []
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      sessionLog,
      maxTokens: 8000,
      subAgentOrchestrator: {
        async run(ctx: { spec: { tokenBudget?: number; maxTurns?: number; maxWallMs?: number }; manifest: { agent_id: string } }) {
          seen.push({ tokenBudget: ctx.spec.tokenBudget, maxTurns: ctx.spec.maxTurns, maxWallMs: ctx.spec.maxWallMs })
          return {
            agentId: ctx.manifest.agent_id,
            result: {
              termination: "completed",
              finalMessage: { role: "assistant", content: "bounded", toolCalls: [] },
              turnsUsed: 1,
              totalTokensUsed: 1,
            },
          }
        },
      } as never,
    } as never)

    const outcome = await runner.runWorkflow({
      nodes: [{ task: "bounded", role: "implement", tokenBudget: 5000, maxTurns: 4, maxWallMs: 30_000 }],
    }, { sessionId: "caps-debug" })

    expect(outcome.nodeOutcomes[0]?.status).toBe("completed")
    expect(seen).toEqual([{ tokenBudget: 5000, maxTurns: 4, maxWallMs: 30_000 }])
  })
})

describe("W-N1: workflow nodes get tools (trusted inherit; quarantined stay deny-all)", () => {
  function nodeProvider(): LLMProvider {
    let call = 0
    return {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        call += 1
        if (call === 1) {
          yield { type: "tool_call", id: `t-${call}`, name: "ping", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "node done" }
      },
    }
  }

  it("a trusted workflow node can call the parent's registered tools", async () => {
    let pings = 0
    const ping = tool("ping", "ping the host", { type: "object", properties: {} }, async () => {
      pings += 1
      return "pong"
    })
    const { runner } = createRunner(nodeProvider(), [ping])
    const outcome = await runner.runWorkflow({ nodes: [{ task: "use the ping tool once, then stop", role: "implement" }] })
    expect(outcome.nodeOutcomes).toEqual([expect.objectContaining({ nodeId: "wf-node0", status: "completed" })])
    expect(pings).toBe(1) // pre-W-N1 this was 0: the missing grant list ran every node TOOL-LESS
  })

  it("fails closed until canonical WorkflowNode can represent quarantine", async () => {
    let pings = 0
    const ping = tool("ping", "ping the host", { type: "object", properties: {} }, async () => {
      pings += 1
      return "pong"
    })
    const { runner } = createRunner(nodeProvider(), [ping])
    const outcome = await runner.runWorkflow({
      nodes: [{ task: "try the ping tool", role: "explore", isolation: "read_only", trust: "quarantined" }],
    })
    expect(outcome.nodeOutcomes).toEqual([])
    expect(outcome.rejection?.reason).toContain("absent from canonical WorkflowNode: trust")
    expect(pings).toBe(0)
  })
})

describe("W-N5: ReactiveSession.resume rebuilds peers, not vehicles", () => {
  it("restores only explicitly tagged peer memberships", async () => {
    const store = new InMemoryGroupBudgetStore()
    store.join("g1", { sessionId: "alice", role: "reviewer", kind: "peer" })
    store.join("g1", { sessionId: "wf-abc123", role: "loop", kind: "vehicle" })
    store.join("g1", { sessionId: "bob", kind: "peer" })
    const session = await ReactiveSession.resume({
      runGroup: { id: "g1", budgetStore: store },
      turnPolicy: async () => [],
      makeRunner: () => { throw new Error("not driven in this test") },
    })
    expect(session.peers().sort()).toEqual(["alice", "bob"])

    const untagged = new InMemoryGroupBudgetStore()
    untagged.join("g2", { sessionId: "solo" })
    const strictSession = await ReactiveSession.resume({
      runGroup: { id: "g2", budgetStore: untagged },
      turnPolicy: async () => [],
      makeRunner: () => { throw new Error("not driven in this test") },
    })
    expect(strictSession.peers()).toEqual([])
  })
})
