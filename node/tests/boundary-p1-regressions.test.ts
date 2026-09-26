/**
 * Node SDK → canonical kernel boundary regressions (P1 audit, 0.2.74).
 *
 * P1-7:  the host re-applied the model's `update_plan` and wrote its own "Executed tools" progress
 *        into the model's task state after every tool batch.
 * P1-10: launch / preemption acknowledgements echoed "the last action" as unconditionally started,
 *        child usage was fabricated (`input_tokens: "0"`, total reported as output), `cancelled`
 *        was never reported, and a missing attempt id was invented as `<task>:attempt:1`.
 * P1-9:  the host rewrote the kernel-rendered tool surface and knowledge after the kernel committed
 *        them (governance prefilter, with last-match rule semantics the gate does not use).
 * P1-11: signal deadlines were dropped, a host clock was stamped on the wire, and injected notes
 *        were recognised by an `injected-` delivery-id prefix.
 * P1-14: the kernel's live control plane (policy patch, deadline, forced compaction, host skill
 *        activation) had no SDK surface; governance constraints were lowered with field names the
 *        kernel rejects.
 * P1-12: `pending_call_ids` carried provider effect ids and sub-agent ids.
 * P1-13: tool-call argument text that was not a JSON object was rewritten to `{}`, so a truncated
 *        call ran the tool with empty arguments.
 * P1-15: memory had two authorities — the host re-implemented write validation, skipped the write
 *        quota, and computed `recall_count + 1` and promotion itself, while model recalls were
 *        never counted at all.
 */
import { getKernel } from "../src/kernel.js"
import { InMemoryKernelJournal } from "../src/runtime/kernel-journal.js"
import { CanonicalRunnerRuntime } from "../src/runtime/canonical-kernel-step.js"
import { normalizeToolCall } from "../src/providers/base.js"
import { governancePolicyPatch } from "../src/governance.js"
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../src/types.js"
import type { MemoryRecallLifecycle, MemoryStore } from "../src/memory/protocols.js"

class ScriptedProvider implements LLMProvider {
  readonly contexts: RenderedContext[] = []
  constructor(private readonly turns: StreamEvent[][]) {}
  async complete(): Promise<ModelMessage> {
    return { role: "assistant", content: "", toolCalls: [] }
  }
  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    this.contexts.push(context)
    const turn = this.turns[this.contexts.length - 1] ?? [{ type: "text_delta", delta: "done" } as StreamEvent]
    for (const event of turn) yield event
  }
}

function contextText(context: RenderedContext): string {
  return JSON.stringify(context)
}

describe("P1-13: malformed tool arguments fail the call instead of running it with {}", () => {
  it("keeps non-object argument text verbatim when normalizing", () => {
    expect(normalizeToolCall("c1", "write", "{\"path\": \"/tm")?.arguments).toBe("{\"path\": \"/tm")
    expect(normalizeToolCall("c1", "write", "[1]")?.arguments).toBe("[1]")
    expect(normalizeToolCall("c1", "write", "")?.arguments).toBe("{}")
    expect(normalizeToolCall("c1", "write", "{ \"a\" : 1 }")?.arguments).toBe("{\"a\":1}")
  })

  it("never executes a truncated call and tells the model why", async () => {
    const executed: unknown[] = []
    const plane = new LocalExecutionPlane()
    plane.register(tool("write", "write a file", { type: "object", properties: { path: { type: "string" } } }, async args => {
      executed.push(args)
      return "wrote"
    }))
    const provider = new ScriptedProvider([[{
      type: "tool_call", id: "call_trunc", name: "write", arguments: {}, rawArguments: "{\"path\": \"/tm",
    } as StreamEvent]])
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 4,
      baselineToolIds: ["write"],
    } as never)
    const events: StreamEvent[] = []
    for await (const event of runner.run({ sessionId: "p1-13", goal: "write it" })) events.push(event)

    expect(executed).toEqual([])
    const result = events.find(event => event.type === "tool_result") as { isError?: boolean; content?: string } | undefined
    expect(result?.isError).toBe(true)
    expect(result?.content).toMatch(/invalid arguments/)
    expect(provider.contexts.length).toBeGreaterThanOrEqual(2)
    expect(contextText(provider.contexts[1])).toContain("invalid arguments")
  })
})

describe("a failed tool result reaches the next provider request marked as a failure", () => {
  it("carries is_error through the kernel's rendered context", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("fetch", "fetch a page", { type: "object", properties: {} }, async () => {
      throw new Error("upstream timeout")
    }))
    const provider = new ScriptedProvider([[{ type: "tool_call", id: "call_fail", name: "fetch", arguments: {} } as StreamEvent]])
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 4,
      baselineToolIds: ["fetch"],
    } as never)
    for await (const _ of runner.run({ sessionId: "is-error", goal: "fetch it" })) void _
    const parts = provider.contexts[1].turns.flatMap(turn => turn.contentParts ?? [])
    const result = parts.find(part => part.type === "tool_result" && part.callId === "call_fail") as { isError?: boolean } | undefined
    expect(result?.isError).toBe(true)
  })
})

describe("P1-12: cancellation names logical calls only", () => {
  it("does not put a provider effect id into pending_call_ids", async () => {
    const plane = new LocalExecutionPlane()
    let runner: RuntimeRunner | undefined
    plane.register(tool("stop_me", "interrupts the run", { type: "object", properties: {} }, async () => {
      runner?.interrupt("user")
      return "ok"
    }))
    const provider = new ScriptedProvider([[{ type: "tool_call", id: "call_stop", name: "stop_me", arguments: {} } as StreamEvent]])
    const sessionLog = new InMemorySessionLog()
    runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 4,
      baselineToolIds: ["stop_me"],
    } as never)
    for await (const _event of runner.run({ sessionId: "p1-12", goal: "stop" })) { /* drain */ }

    const records = JSON.parse(JSON.stringify(await sessionLog.read("p1-12"))) as unknown[]
    const cancelled = findDeep(records, value => value.kind === "operation_cancelled")
    expect(cancelled).toBeDefined()
    expect(cancelled?.pending_call_ids).toEqual([])
  })
})

function findDeep(value: unknown, match: (value: Record<string, unknown>) => boolean): Record<string, unknown> | undefined {
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findDeep(item, match)
      if (found) return found
    }
    return undefined
  }
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>
    if (match(record)) return record
    for (const item of Object.values(record)) {
      const found = findDeep(item, match)
      if (found) return found
    }
  }
  return undefined
}

describe("P1-7: the host never writes the model's task state", () => {
  it("keeps the model's update_plan progress after a tool batch", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("noop", "does nothing", { type: "object", properties: {} }, async () => "ok"))
    const provider = new ScriptedProvider([
      [{ type: "tool_call", id: "call_plan", name: "update_plan", arguments: { progress: "drafting section two" } } as StreamEvent],
      [{ type: "tool_call", id: "call_noop", name: "noop", arguments: {} } as StreamEvent],
    ])
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 5,
      baselineToolIds: ["noop"],
      enablePlanTool: true,
    } as never)
    const events: StreamEvent[] = []
    for await (const event of runner.run({ sessionId: "p1-7", goal: "write" })) events.push(event)

    expect(events.filter(event => event.type === "error")).toEqual([])
    expect(provider.contexts.length).toBeGreaterThanOrEqual(3)
    const third = contextText(provider.contexts[2])
    expect(third).toContain("drafting section two")
    expect(third).not.toContain("Executed tools")
  })
})

describe("P1-10: launch and completion facts are the host's observations", () => {
  async function spawnTwo(journal = new InMemoryKernelJournal(), id = "spawn") {
    const { CanonicalKernel } = getKernel()
    const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), journal, `op-${id}`, { maxContextTokens: 100_000 })
    await rt.applyHostEvent({ kind: "configure_run", config: { resource_quota: { max_concurrent_subagents: 4 } } })
    await rt.startDynamicWorkflow()
    const action = await rt.appendWorkflowNodes({
      nodes: [{ nodeId: "a", task: "a", role: "implement" }, { nodeId: "b", task: "b", role: "implement" }],
    })
    if (action?.kind !== "spawn_workflow") throw new Error(`expected spawn_workflow, got ${action?.kind}`)
    rt.drainHostObservations()
    return { rt, action, journal }
  }

  it("reports a task the host failed to launch as failed, not started", async () => {
    const { rt, action } = await spawnTwo()
    await rt.applyHostEvent({
      kind: "workflow_spawn_result",
      effect_id: action.effectId,
      started_agent_ids: ["wf-node0"],
      failures: [{ agent_id: "wf-node1", error: "no worker slot", kind: "resource_exhausted" }],
    })
    const spawned = rt.drainHostObservations().find(obs => obs.kind === "workflow_batch_spawned") as
      | { nodes?: Array<{ agent_id: string }> } | undefined
    expect(spawned?.nodes?.map(node => node.agent_id)).toEqual(["wf-node0"])
  })

  it("refuses to acknowledge an effect id that is not a pending launch", async () => {
    const { rt } = await spawnTwo(undefined, "spawn-unknown")
    await expect(rt.applyHostEvent({ kind: "workflow_spawn_result", effect_id: "not-an-effect" }))
      .rejects.toThrow(/not a pending spawn_tasks effect/)
  })

  it("never invents an attempt id for a completion it cannot attribute", async () => {
    const { rt, action, journal } = await spawnTwo(undefined, "spawn-restore")
    await rt.applyHostEvent({ kind: "workflow_spawn_result", effect_id: action.effectId })
    // A restored runtime has not seen the launch acknowledgement, so only the host's echo of the
    // spawned node's attempt id can attribute the completion.
    const { CanonicalKernel } = getKernel()
    const restored = new CanonicalRunnerRuntime(new CanonicalKernel(), journal, "op-spawn-restore", { maxContextTokens: 100_000 })
    await restored.restore()
    const completion = {
      kind: "sub_agent_completed",
      result: { agent_id: "wf-node0", result: { termination: "completed", turns_used: 1, total_tokens_used: 10 } },
    }
    await expect(restored.applyHostEvent(completion)).rejects.toThrow(/no kernel-minted attempt id/)
    await expect(restored.applyHostEvent({ ...completion, attempt_id: String(action.nodes[0].attempt_id) }))
      .resolves.not.toThrow()
    const joined = restored.drainHostObservations().find(obs => obs.kind === "agent_process_changed") as
      | { state?: string } | undefined
    expect(joined?.state).toBe("joined")
  })
})

describe("P1-9: the provider sees exactly the context and surface the kernel committed", () => {
  class RecordingProvider extends ScriptedProvider {
    readonly toolNames: string[][] = []
    constructor() { super([]) }
    override async *stream(context: RenderedContext, tools: Array<{ name: string }>): AsyncIterable<StreamEvent> {
      this.toolNames.push(tools.map(t => t.name))
      yield* super.stream(context)
    }
  }

  async function firstRequest(surfaceDeniedInSystem?: boolean) {
    const plane = new LocalExecutionPlane()
    plane.register(tool("rm", "remove", { type: "object", properties: {} }, async () => "gone"))
    plane.register(tool("ls", "list", { type: "object", properties: {} }, async () => "files"))
    const provider = new RecordingProvider()
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 2,
      baselineToolIds: ["rm", "ls"],
      governancePolicy: {
        vetoes: ["rm"],
        ...(surfaceDeniedInSystem !== undefined ? { surfaceDeniedInSystem } : {}),
      },
    } as never)
    for await (const _event of runner.run({ sessionId: `p1-9-${String(surfaceDeniedInSystem)}`, goal: "tidy" })) { /* drain */ }
    return { tools: provider.toolNames[0] ?? [], knowledge: provider.contexts[0]?.systemKnowledge ?? "" }
  }

  it("hides vetoed tools through the kernel, with the note rendered once", async () => {
    const hidden = await firstRequest()
    expect(hidden.tools).toContain("ls")
    expect(hidden.tools).not.toContain("rm")
    expect(hidden.knowledge.match(/\[governance\]/g)?.length).toBe(1)
    expect(hidden.knowledge).toContain("rm")

    const shown = await firstRequest(false)
    expect(shown.tools).toContain("rm")
    expect(shown.knowledge).not.toContain("[governance]")
  })
})

describe("P1-11: signals reach the kernel without dropped fields or id side channels", () => {
  it("admits a deadline-bearing signal and a host note through the real kernel", async () => {
    const { signalToKernelEvent } = await import("../src/runtime/runner.js")
    const { CanonicalKernel } = getKernel()
    const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-signal", { maxContextTokens: 100_000 })
    const first = await rt.startAgent({ goal: "wait for signals" })
    if (first?.kind !== "call_provider") throw new Error(`expected call_provider, got ${first?.kind}`)
    for (const delivery of [
      {
        signalId: "sig-deadline", deliveryId: "delivery-1", deliveryAttempt: 1,
        signal: { source: "cron" as const, signalType: "job" as const, urgency: "low" as const, payload: { job: "nightly" }, deadlineMs: 2_000 },
      },
      {
        signalId: "sig-note", deliveryId: "delivery-2", deliveryAttempt: 1,
        signal: { source: "custom" as const, signalType: "event" as const, urgency: "normal" as const, payload: {} },
        note: "the deploy finished",
      },
    ]) {
      await rt.applyHostEvent(signalToKernelEvent({ ...delivery, nowMs: 1_000 }))
    }
    const disposed = rt.drainHostObservations().filter(obs => obs.kind === "signal_delivery_disposed")
    expect(disposed).toHaveLength(2)
  })
})

describe("P1-14: the kernel's live control plane is reachable from the runner", () => {
  function controlledRun(opts: Record<string, unknown>, onTool: (runner: RuntimeRunner) => void, turns = 3) {
    const plane = new LocalExecutionPlane()
    let runner: RuntimeRunner | undefined
    plane.register(tool("poke", "poke", { type: "object", properties: {} }, async () => {
      onTool(runner!)
      return "poked"
    }))
    const script: StreamEvent[][] = Array.from({ length: turns }, (_, i) =>
      [{ type: "tool_call", id: `call_${i}`, name: "poke", arguments: { i } } as StreamEvent])
    const provider = new ScriptedProvider(script)
    const sessionLog = new InMemorySessionLog()
    runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 6,
      baselineToolIds: ["poke"],
      repeatFuse: false,
      ...opts,
    } as never)
    return { runner, provider, sessionLog }
  }

  it("lowers governance constraints the kernel accepts", async () => {
    const { runner } = controlledRun({
      governancePolicy: {
        constraints: [
          { kind: "required", tool: "poke", path: "i" },
          { kind: "range", tool: "poke", path: "i", min: 0, max: 10.5 },
        ],
      },
    }, () => {})
    const events: StreamEvent[] = []
    for await (const event of runner.run({ sessionId: "p1-14-constraints", goal: "poke" })) events.push(event)
    expect(events.filter(event => event.type === "error")).toEqual([])
  })

  it("applies a revision-guarded governance patch mid-run", async () => {
    const patches: Array<Promise<void>> = []
    const { runner, provider } = controlledRun({}, current => {
      if (patches.length === 0) patches.push(current.applyPolicyPatch(governancePolicyPatch({ vetoes: ["poke"] })))
    })
    for await (const _event of runner.run({ sessionId: "p1-14-patch", goal: "poke" })) { /* drain */ }
    await expect(patches[0]).resolves.toBeUndefined()
    // After the patch the kernel withholds the vetoed tool and says so.
    const later = provider.contexts[provider.contexts.length - 1]
    expect(later.systemKnowledge).toContain("[governance]")
  })

  it("refuses a patch at a stale revision", async () => {
    const results: Array<Promise<void>> = []
    const { runner } = controlledRun({}, current => {
      if (results.length === 0) {
        results.push(current.applyPolicyPatch(governancePolicyPatch({ vetoes: ["x"] }), { expectedRevision: 7 }))
      }
    })
    for await (const _event of runner.run({ sessionId: "p1-14-stale", goal: "poke" })) { /* drain */ }
    await expect(results[0]).rejects.toThrow(/revision mismatch/)
  })

  it("moves the deadline and forces a compaction through the kernel", async () => {
    const commands: Array<Promise<void>> = []
    const { runner, sessionLog } = controlledRun({}, current => {
      if (commands.length === 0) {
        commands.push(current.forceCompact())
        commands.push(current.updateDeadline(0))
      }
    })
    const events: StreamEvent[] = []
    for await (const event of runner.run({ sessionId: "p1-14-deadline", goal: "poke" })) events.push(event)
    await expect(Promise.all(commands)).resolves.toBeDefined()
    const done = events.find(event => event.type === "done") as { status?: string } | undefined
    expect(done?.status).toBe("deadline")
    void sessionLog
  })

  it("rejects a control command when no run is active", async () => {
    const { runner } = controlledRun({}, () => {})
    await expect(runner.forceCompact()).rejects.toThrow(/requires an active run/)
  })
})

describe("P1-15: memory has one authority — the kernel", () => {
  const scope = { tenant_id: "p1-15", namespace: "memory" }
  function stored(recordId: string, recallCount: number, pinned = false) {
    return {
      record_id: recordId, scope, name: recordId, kind: "reference" as const, content: `${recordId} body`,
      description: "fixture", provenance: { author: "host" as const, trust: "host_verified" as const, evidence_refs: [] },
      created_at: 1, updated_at: 1, recall_count: recallCount, confidence: 1, links: [], pinned,
    }
  }
  function store(hits: ReturnType<typeof stored>[] = []) {
    const puts: string[] = []
    const recalls: MemoryRecallLifecycle[][] = []
    const memoryStore: MemoryStore = {
      put: async (_agent, record) => { puts.push(record.record_id) },
      get: async () => null,
      delete: async () => {},
      saveSession: async () => {},
      search: async () => hits.map(record => ({ record, score: 0.9, why: "fixture" })),
      recordRecall: async (_agent, batch) => { recalls.push(batch) },
    }
    return { memoryStore, puts, recalls }
  }

  it("mirrors the recall count the kernel derived for a model memory query", async () => {
    const { memoryStore, recalls } = store([stored("crossing", 2)])
    const promotions: unknown[] = []
    const provider = new ScriptedProvider([[{ type: "tool_call", id: "call_mem", name: "memory", arguments: { query: "prefs" } } as StreamEvent]])
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      maxTurns: 4,
      agentId: "p1-15",
      memoryScope: scope,
      memoryStore,
      preQueryMemory: () => [],
      memoryPolicy: { promotionRecallThreshold: 3 },
      onPromotionSuggested: (promotion: unknown) => promotions.push(promotion),
    } as never)
    for await (const _ of runner.run({ sessionId: "p1-15-query", goal: "recall" })) void _

    expect(recalls.flat().map(recall => [recall.record_id, recall.recall_count])).toEqual([["crossing", 3]])
    expect(promotions).toEqual([{ recordId: "crossing", recallCount: 3 }])
  })

  it("admits a host write inside a live run by the kernel's rolling write quota", async () => {
    const { memoryStore, puts } = store()
    const verdicts: boolean[] = []
    let runner: RuntimeRunner | undefined
    const plane = new LocalExecutionPlane()
    plane.register(tool("remember_twice", "writes two memories", { type: "object", properties: {} }, async () => {
      for (const id of ["first", "second"]) verdicts.push(await runner!.writeMemory(stored(id, 0)))
      return "ok"
    }))
    runner = new RuntimeRunner({
      provider: new ScriptedProvider([[{ type: "tool_call", id: "call_w", name: "remember_twice", arguments: {} } as StreamEvent]]),
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 4,
      baselineToolIds: ["remember_twice"],
      agentId: "p1-15",
      memoryScope: scope,
      memoryStore,
      preQueryMemory: () => [],
      resourceQuota: { memoryWritesPerWindow: { maxWrites: 1, windowMs: 60_000 } },
    } as never)
    for await (const _ of runner.run({ sessionId: "p1-15-quota", goal: "remember" })) void _

    expect(verdicts).toEqual([true, false])
    expect(puts).toEqual(["first"])
  })

  it("judges a host write with no live run by the kernel's rule", async () => {
    const make = (memoryPolicy?: Record<string, unknown>) => {
      const fixture = store()
      const runner = new RuntimeRunner({
        provider: new ScriptedProvider([]),
        sessionLog: new InMemorySessionLog(),
        agentId: "p1-15",
        memoryStore: fixture.memoryStore,
        ...(memoryPolicy ? { memoryPolicy } : {}),
      } as never)
      return { runner, puts: fixture.puts }
    }
    // the name limit counts characters, not UTF-8 bytes
    const wide = { ...stored("wide", 0), name: "记".repeat(100) }
    expect(await make().runner.writeMemory(wide)).toBe(true)
    expect(await make().runner.writeMemory({ ...wide, name: "记".repeat(101) })).toBe(false)
    expect(await make({ maxContentBytes: 4 }).runner.writeMemory({ ...stored("big", 0), content: "12345" })).toBe(false)
    expect(await make({ maxContentBytes: 4, validationEnabled: false }).runner.writeMemory({ ...stored("big", 0), content: "12345" })).toBe(true)
  })

  it("derives a host recall's counts in the kernel", async () => {
    const { memoryStore, recalls } = store([stored("a", 1), stored("a", 1), stored("pinned", 1, true)])
    const promotions: unknown[] = []
    const runner = new RuntimeRunner({
      provider: new ScriptedProvider([]),
      sessionLog: new InMemorySessionLog(),
      agentId: "p1-15",
      memoryStore,
      memoryPolicy: { promotionRecallThreshold: 2 },
      onPromotionSuggested: (promotion: unknown) => promotions.push(promotion),
    } as never)
    await runner.queryMemory({ scope, query: "a", top_k: 5, kinds: [] })

    expect(recalls.flat().map(recall => [recall.record_id, recall.recall_count])).toEqual([["a", 2], ["pinned", 2]])
    expect(promotions).toEqual([{ recordId: "a", recallCount: 2 }])
  })
})
