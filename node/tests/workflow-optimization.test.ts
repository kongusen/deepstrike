/** Dynamic-workflow optimization batch: node-observable kernel behavior and per-node caps. */
import { workflowNodeSpecToKernel, workflowNodeToSpec } from "../src/types/agent.js"
import { dependencyOutputsNote } from "../src/runtime/workflow-control-flow.js"
import { createRunner, tool } from "./runtime/helpers.js"
import { ReactiveSession } from "../src/runtime/reactive-session.js"
import { InMemoryGroupBudgetStore } from "../src/runtime/run-group.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

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

  it("dependencyOutputsNote formats, clips, and skips empty outputs", () => {
    const outputs = new Map([
      ["wf-node0", "alpha findings"],
      ["wf-node1", "x".repeat(9000)],
    ])
    const note = dependencyOutputsNote(["wf-node0", "wf-node1", "wf-node-missing"], outputs, 100)
    expect(note).toContain("[dependency wf-node0 output]\nalpha findings")
    expect(note).toContain("…[truncated]")
    expect(note).not.toContain("wf-node-missing")
    expect(dependencyOutputsNote([], outputs)).toBe("")
    expect(dependencyOutputsNote(undefined, outputs)).toBe("")
  })
})

describe("W-N1: workflow nodes get tools (trusted inherit; quarantined stay deny-all)", () => {
  function nodeProvider(): LLMProvider {
    let call = 0
    return {
      async complete(): Promise<Message> {
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

describe("DW-3/W-N6: loop nodes pace through the kernel trap on ONE stable session", () => {
  /** Per ITERATION the child makes two calls: propose a pace verb, then file the report turn. */
  function pacingLoopProvider(verbs: string[]): LLMProvider {
    let call = 0
    return {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        call += 1
        const iteration = Math.ceil(call / 2) - 1
        if (call % 2 === 1) {
          yield {
            type: "tool_call", id: `pace-${call}`, name: "pace",
            arguments: { next: verbs[Math.min(iteration, verbs.length - 1)], reason: `iter ${iteration}` },
          }
          return
        }
        yield { type: "text_delta", delta: `iteration ${iteration} report` }
      },
    }
  }

  it("fails closed instead of silently running a loop node once", async () => {
    const { runner, sessionLog } = createRunner(pacingLoopProvider(["continue", "stop"]))
    const outcome = await runner.runWorkflow(
      { nodes: [{ task: "polish until done", role: "implement", loop: { maxIters: 5 } }] },
      { sessionId: "wfloop" },
    )
    expect(outcome.nodeOutcomes).toEqual([])
    expect(outcome.rejection?.reason).toContain("absent from canonical WorkflowNode: kind")
    expect(await sessionLog.read("wfloop-wf-node0")).toEqual([])
  })

  it("also rejects a silent loop before starting its child", async () => {
    const silent: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "all done in one pass" }
      },
    }
    const { runner, sessionLog } = createRunner(silent)
    const outcome = await runner.runWorkflow(
      { nodes: [{ task: "one-shot polish", role: "implement", loop: { maxIters: 4 } }] },
      { sessionId: "wfsilent" },
    )
    expect(outcome.nodeOutcomes).toEqual([])
    expect(outcome.rejection?.reason).toContain("absent from canonical WorkflowNode: kind")
    expect(await sessionLog.read("wfsilent-wf-node0")).toEqual([])
  })
})

describe("W-N5: ReactiveSession.resume rebuilds peers, not vehicles", () => {
  it("filters vehicle members and keeps legacy untagged memberships whole", async () => {
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

    // Legacy: nothing tagged → every member resumes as a peer (old behavior preserved).
    const legacy = new InMemoryGroupBudgetStore()
    legacy.join("g2", { sessionId: "solo" })
    const legacySession = await ReactiveSession.resume({
      runGroup: { id: "g2", budgetStore: legacy },
      turnPolicy: async () => [],
      makeRunner: () => { throw new Error("not driven in this test") },
    })
    expect(legacySession.peers()).toEqual(["solo"])
  })
})
