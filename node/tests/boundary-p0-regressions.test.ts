/**
 * Node SDK → canonical kernel boundary regressions (P0 audit, 0.2.74).
 *
 * P0-1: preloaded history kept tool results but dropped the assistant tool calls they answer.
 * P0-2: a model-authored workflow using a kind the canonical DAG cannot express crashed the run
 *       instead of coming back to the model as a syscall rejection.
 * P0-3: caller node ids were discarded and every append restarted at `wf-node0`, so appended nodes
 *       shared one wire identity.
 * P0-4: milestone criteria / required evidence never reached `onMilestoneEvaluate`.
 */
import { getKernel } from "../src/kernel.js"
import { InMemoryKernelJournal } from "../src/runtime/kernel-journal.js"
import { CanonicalRunnerRuntime } from "../src/runtime/canonical-kernel-step.js"
import { messageToKernelMessage } from "../src/runtime/kernel-step.js"
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import { startWorkflowTool } from "../src/types/agent.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

function runtime(id: string): CanonicalRunnerRuntime {
  const { CanonicalKernel } = getKernel()
  return new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), `op-${id}`, {
    maxContextTokens: 100_000,
  })
}

describe("P0-1: preloaded history keeps tool-call pairing", () => {
  it("renders the assistant call ahead of the tool result that answers it", async () => {
    const rt = runtime("history")
    await rt.applyHostEvent({
      kind: "set_tools",
      tools: [{ name: "search", description: "s", parameters: { type: "object", properties: {} } }],
    })
    const history: ModelMessage[] = [
      { role: "user", content: "find x", toolCalls: [] },
      { role: "assistant", content: "", toolCalls: [{ id: "call_1", name: "search", arguments: "{\"q\":\"x\"}" }] },
      { role: "tool", content: "", toolCalls: [], contentParts: [{ type: "tool_result", callId: "call_1", output: "result-x", isError: false }] },
      { role: "assistant", content: "done", toolCalls: [] },
    ]
    await rt.applyHostEvent({ kind: "preload_history", messages: history.map(messageToKernelMessage) })
    const action = await rt.startAgent({ goal: "continue" })
    if (action?.kind !== "call_provider") throw new Error(`expected call_provider, got ${action?.kind}`)

    const turns = action.context.turns
    const callIndex = turns.findIndex(turn => turn.toolCalls?.some(call => call.id === "call_1"))
    const resultIndex = turns.findIndex(turn =>
      turn.contentParts?.some(part => part.type === "tool_result" && part.callId === "call_1"))
    expect(callIndex).toBeGreaterThanOrEqual(0)
    expect(turns[callIndex].toolCalls[0]).toMatchObject({ name: "search", arguments: "{\"q\":\"x\"}" })
    expect(resultIndex).toBeGreaterThan(callIndex)
    // Plain output, not the opaque `[[deepstrike-content-parts]]` base64 envelope.
    expect(turns[resultIndex].content).toBe("result-x")
  })
})

describe("P0-2: an inexpressible model workflow is a rejection, not a crash", () => {
  class LoopAuthor implements LLMProvider {
    turn = 0
    readonly contexts: RenderedContext[] = []
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    }
    async *stream(context: RenderedContext, _tools: ToolSchema[]): AsyncIterable<StreamEvent> {
      this.contexts.push(context)
      this.turn += 1
      if (this.turn === 1) {
        yield {
          type: "tool_call",
          id: "call-loop",
          name: "start_workflow",
          arguments: { spec: { nodes: [{ task: "refine", role: "implement", loop: { maxIters: 2 } }] } },
        } as unknown as StreamEvent
      } else {
        yield { type: "text_delta", delta: "recovered" } as StreamEvent
      }
    }
  }

  it("no longer advertises control-flow kinds the canonical DAG cannot run", () => {
    const schema = startWorkflowTool.parameters
    for (const field of ["loop", "classify", "tournament", "reducer", "depPolicy", "quarantined"]) {
      expect(schema).not.toContain(`"${field}"`)
    }
  })

  it("answers the model with a kernel rejection and lets the run continue", async () => {
    const provider = new LoopAuthor()
    const plane = new LocalExecutionPlane()
    plane.register(tool(startWorkflowTool.name, startWorkflowTool.description, JSON.parse(startWorkflowTool.parameters), async () => ""))
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 5,
      baselineToolIds: ["start_workflow"],
    } as never)

    const events: StreamEvent[] = []
    for await (const event of runner.run({ sessionId: "p0-2", goal: "loop on it" })) events.push(event)

    expect(events.filter(event => event.type === "error")).toEqual([])
    expect(provider.turn).toBeGreaterThanOrEqual(2)
    const second = provider.contexts[1]
    const text = [second.systemKnowledge, second.stateTurn?.content, ...second.turns.map(turn => turn.content)]
      .filter(Boolean).join("\n")
    expect(text).toContain("start_workflow")
    const done = events.find(event => event.type === "done") as { status?: string } | undefined
    expect(done?.status).not.toBe("error")
  })
})

describe("P0-3: workflow node identity survives appends", () => {
  it("keeps caller node ids and gives anonymous appended nodes unique ids", async () => {
    const rt = runtime("dynamic")
    await rt.applyHostEvent({ kind: "configure_run", config: { resource_quota: { max_concurrent_subagents: 4 } } })
    expect(await rt.startDynamicWorkflow()).toBeNull()

    const spawned: Array<{ task_id?: string; node_id?: string }> = []
    for (const nodes of [
      [{ nodeId: "alpha", task: "a", role: "implement" }],
      [{ nodeId: "beta", task: "b", role: "implement" }],
      [{ task: "anonymous", role: "implement" }],
    ]) {
      const action = await rt.appendWorkflowNodes({ nodes })
      if (action?.kind !== "spawn_workflow") throw new Error(`expected spawn_workflow, got ${action?.kind}`)
      spawned.push(...action.nodes.map(node => ({ task_id: node.task_id, node_id: node.node_id })))
      await rt.applyHostEvent({ kind: "workflow_spawn_result", effect_id: action.effectId })
    }
    expect(spawned).toEqual([
      { task_id: "wf-node0", node_id: "alpha" },
      { task_id: "wf-node1", node_id: "beta" },
      { task_id: "wf-node2", node_id: "wf-node2" },
    ])
  })

  it("refuses a node id the DAG already declares", async () => {
    const rt = runtime("duplicate")
    await rt.applyHostEvent({ kind: "configure_run", config: { resource_quota: { max_concurrent_subagents: 4 } } })
    await rt.startDynamicWorkflow()
    const first = await rt.appendWorkflowNodes({ nodes: [{ nodeId: "alpha", task: "a", role: "implement" }] })
    if (first?.kind !== "spawn_workflow") throw new Error("expected spawn_workflow")
    await rt.applyHostEvent({ kind: "workflow_spawn_result", effect_id: first.effectId })
    await expect(rt.appendWorkflowNodes({ nodes: [{ nodeId: "alpha", task: "again", role: "implement" }] }))
      .rejects.toThrow(/already declared/)
  })
})

describe("P0-4: milestone evaluation receives the phase's host-side requirements", () => {
  it("passes criteria and required evidence to onMilestoneEvaluate", async () => {
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "done" } as StreamEvent
      },
    }
    const seen: Array<{ phaseId: string; criteria: string[]; requiredEvidence: string[] }> = []
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4000,
      maxTurns: 4,
      milestoneContract: {
        phases: [{ id: "phase1", criteria: ["tests pass"], requiredEvidence: ["test log"] }],
      },
      onMilestoneEvaluate: (ctx: { phaseId: string; criteria: string[]; requiredEvidence: string[] }) => {
        seen.push(ctx)
        return { phaseId: ctx.phaseId, passed: true }
      },
    } as never)

    for await (const _event of runner.run({ sessionId: "p0-4", goal: "ship it" })) { /* drain */ }

    expect(seen.length).toBeGreaterThanOrEqual(1)
    expect(seen[0]).toEqual({ phaseId: "phase1", criteria: ["tests pass"], requiredEvidence: ["test log"] })
  })
})

describe("P0-5: onToolResult redaction covers every consumer", () => {
  class SecretPlane {
    register(): this { return this }
    unregister(): this { return this }
    schemas(): ToolSchema[] {
      return [{ name: "fetch", description: "fetch", parameters: '{"type":"object"}' }]
    }
    async *executeAll(calls: Array<{ id: string; name: string }>): AsyncIterable<StreamEvent> {
      for (const call of calls) {
        yield {
          type: "tool_result",
          callId: call.id,
          name: call.name,
          content: "SECRET-TOKEN",
          isError: false,
          contentParts: [{ type: "text", text: "SECRET-TOKEN" }],
        } as StreamEvent
      }
    }
  }

  class FetchThenStop implements LLMProvider {
    readonly contexts: RenderedContext[] = []
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    }
    async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
      this.contexts.push(context)
      if (this.contexts.length === 1) {
        yield { type: "tool_call", id: "call_fetch", name: "fetch", arguments: {} } as StreamEvent
      } else {
        yield { type: "text_delta", delta: "done" } as StreamEvent
      }
    }
  }

  it("keeps the redacted structured blocks out of the next request and the session record", async () => {
    const provider = new FetchThenStop()
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new SecretPlane(),
      maxTokens: 8000,
      maxTurns: 4,
      baselineToolIds: ["fetch"],
      onToolResult: () => ({ replaceOutput: "[redacted]" }),
    } as never)
    for await (const _event of runner.run({ sessionId: "p0-5", goal: "fetch it" })) { /* drain */ }

    expect(provider.contexts.length).toBeGreaterThanOrEqual(2)
    expect(JSON.stringify(provider.contexts[1])).not.toContain("SECRET-TOKEN")
    expect(JSON.stringify(provider.contexts[1])).toContain("[redacted]")
    const logged = JSON.stringify(await sessionLog.read("p0-5"))
    expect(logged).not.toContain("SECRET-TOKEN")
  })
})

describe("P0-6: host resolutions never mint a receipt", () => {
  /** Drive tool turns under a small budget until context pressure publishes a page-out. */
  async function pressured(id: string) {
    const { CanonicalKernel } = getKernel()
    const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), `op-${id}`, {
      maxContextTokens: 4_000,
    })
    await rt.applyHostEvent({ kind: "set_tools", tools: [{ name: "ping", description: "ping", parameters: { type: "object" } }] })
    let action = await rt.startAgent({ goal: "keep pinging" }, { goal: "keep pinging", exposure_baseline: ["ping"] })
    for (let turn = 0; turn < 40 && action; turn += 1) {
      if (action.kind === "archive_page_out") return { rt, action }
      if (action.kind === "call_provider") {
        action = await rt.applyHostEvent({
          kind: "provider_result",
          effect_id: action.effectId,
          message: { role: "assistant", content: "", tool_calls: [{ id: `call_${turn}`, name: "ping", arguments: { n: turn } }] },
          observed_input_tokens: 3_900,
          stop_reason: "tool_use",
        })
      } else if (action.kind === "execute_tool") {
        action = await rt.applyHostEvent({
          kind: "tool_results",
          effect_id: action.effectId,
          results: [{ call_id: action.calls[0].id, output: `pong ${turn}: ${"a long tool body worth compacting ".repeat(20)}`, is_error: false }],
        })
      } else {
        throw new Error(`unexpected effect while building pressure: ${action.kind}`)
      }
    }
    throw new Error("context pressure never published an archive_page_out")
  }

  it("fails a page-out whose store returned no ref instead of inventing one", async () => {
    const missing = await pressured("page-out-missing")
    await missing.rt.applyHostEvent({ kind: "page_out_archive_result", effect_id: missing.action!.effectId })
    const failedObs = missing.rt.drainHostObservations().map(obs => obs.kind)
    expect(failedObs).toContain("page_out_archive_failed")
    expect(failedObs).not.toContain("payload_residency_changed")

    const stored = await pressured("page-out-stored")
    await stored.rt.applyHostEvent({ kind: "page_out_archive_result", effect_id: stored.action!.effectId, payload_ref: "payload:stored" })
    const residency = stored.rt.drainHostObservations().find(obs => obs.kind === "payload_residency_changed") as
      | { payload_ref?: string } | undefined
    expect(residency?.payload_ref).toBe("payload:stored")
  })
})
