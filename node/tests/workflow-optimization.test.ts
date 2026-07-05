/**
 * Dynamic-workflow optimization batch (W-N1/N2, W-1 resume signal replay, per-node caps):
 * the node-observable halves of the kernel audit fixes.
 */
import { getKernel } from "../src/kernel.js"
import { workflowNodeSpecToKernel, workflowNodeToSpec } from "../src/types/agent.js"
import type { WorkflowSpawnInfo } from "../src/types/agent.js"
import { dependencyOutputsNote } from "../src/runtime/workflow-control-flow.js"
import { createRunner, tool } from "./runtime/helpers.js"
import { ReactiveSession } from "../src/runtime/reactive-session.js"
import { InMemoryGroupBudgetStore } from "../src/runtime/run-group.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return JSON.parse(rt.step(JSON.stringify({ version: 1, event }))) as {
    observations: Array<{ kind: string; nodes?: WorkflowSpawnInfo[]; completed?: string[]; failed?: string[] }>
  }
}
const batchOf = (obs: ReturnType<typeof step>["observations"]): WorkflowSpawnInfo[] =>
  obs.find(o => o.kind === "workflow_batch_spawned")?.nodes ?? []

describe("W-1: resume replays classify control flow over the ABI", () => {
  it("a recorded classify_branch re-prunes the rejected branch on resume", () => {
    const kernel = getKernel()
    const rt = new kernel.KernelRuntime({ maxTokens: 8000, maxTurns: 10 })
    step(rt, { kind: "start_run", task: { goal: "resume classify", criteria: [] } })
    const out = step(rt, {
      kind: "load_workflow",
      spec: {
        nodes: [
          {
            task: { goal: "route", criteria: [] },
            role: "plan", isolation: "shared", context_inheritance: "none",
            kind: { type: "classify", branches: [{ label: "a", nodes: [1] }, { label: "b", nodes: [2] }] },
          },
          { task: { goal: "on a", criteria: [] }, role: "implement", isolation: "shared", context_inheritance: "none", depends_on: [0] },
          { task: { goal: "on b", criteria: [] }, role: "implement", isolation: "shared", context_inheritance: "none", depends_on: [0] },
        ],
      },
      parent_session_id: "sess",
      // W-1: the signal-carrying record — the classifier chose "a" pre-crash.
      resumed_results: [{ agent_id: "wf-node0", classify_branch: "a" }],
    })
    // Only the chosen branch spawns; the rejected branch stays pruned across resume.
    const batch = batchOf(out.observations)
    expect(batch.map(n => n.agent_id)).toEqual(["wf-node1"])
  })
})

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

  it("a plain dependent node's spawn info carries its dependencies' agent ids", () => {
    const kernel = getKernel()
    const rt = new kernel.KernelRuntime({ maxTokens: 8000, maxTurns: 10 })
    step(rt, { kind: "start_run", task: { goal: "deps", criteria: [] } })
    const out = step(rt, {
      kind: "load_workflow",
      spec: {
        nodes: [
          { task: { goal: "w0", criteria: [] }, role: "explore", isolation: "read_only", context_inheritance: "none" },
          { task: { goal: "w1", criteria: [] }, role: "explore", isolation: "read_only", context_inheritance: "none" },
          { task: { goal: "synth", criteria: [] }, role: "plan", isolation: "shared", context_inheritance: "none", depends_on: [0, 1] },
        ],
      },
      parent_session_id: "sess",
    })
    const workers = batchOf(out.observations)
    expect(workers.map(n => n.input_agent_ids ?? [])).toEqual([[], []])
    // Complete both workers → the synthesizer spawns WITH its data edges.
    const mkResult = (agentId: string) => ({
      kind: "sub_agent_completed",
      result: { agent_id: agentId, result: { termination: "completed", final_message: { role: "assistant", content: `${agentId} out` }, turns_used: 1, total_tokens_used: 1 } },
    })
    step(rt, mkResult("wf-node0"))
    const after = step(rt, mkResult("wf-node1"))
    const synth = batchOf(after.observations)
    expect(synth.map(n => n.agent_id)).toEqual(["wf-node2"])
    expect(synth[0].input_agent_ids).toEqual(["wf-node0", "wf-node1"])
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
    expect(outcome.completed).toEqual(["wf-node0"])
    expect(pings).toBe(1) // pre-W-N1 this was 0: the missing grant list ran every node TOOL-LESS
  })

  it("a quarantined workflow node stays deny-all filtered", async () => {
    let pings = 0
    const ping = tool("ping", "ping the host", { type: "object", properties: {} }, async () => {
      pings += 1
      return "pong"
    })
    const { runner } = createRunner(nodeProvider(), [ping])
    const outcome = await runner.runWorkflow({
      nodes: [{ task: "try the ping tool", role: "explore", isolation: "read_only", trust: "quarantined" }],
    })
    expect(outcome.completed).toEqual(["wf-node0"])
    expect(pings).toBe(0) // untrusted-content reader: no tool reaches the host
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

  it("pace continue→stop drives the iterations; the transcript accumulates under one session id", async () => {
    const { runner, sessionLog } = createRunner(pacingLoopProvider(["continue", "stop"]))
    const outcome = await runner.runWorkflow(
      { nodes: [{ task: "polish until done", role: "implement", loop: { maxIters: 5 } }] },
      { sessionId: "wfloop" },
    )
    expect(outcome.completed).toEqual(["wf-node0"])
    // The pace verb ended the loop at 2 iterations, well before maxIters=5.
    const loopSession = await sessionLog.read("wfloop-wf-node0")
    const starts = loopSession.filter(e => e.event.kind === "run_started")
    expect(starts.length).toBe(2) // W-N6: BOTH iterations ran under the ONE stable session id
    // No per-iteration session fragments.
    expect(await sessionLog.read("wfloop-wf-node0-i0")).toEqual([])
    expect(await sessionLog.read("wfloop-wf-node0-i1")).toEqual([])
  })

  it("an iteration that never paces completes the loop (silence = done, not run-to-cap)", async () => {
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
    expect(outcome.completed).toEqual(["wf-node0"])
    // default_action=stop: exactly ONE iteration ran (the kernel's pace fallback said stop).
    const starts = (await sessionLog.read("wfsilent-wf-node0")).filter(e => e.event.kind === "run_started")
    expect(starts.length).toBe(1)
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
