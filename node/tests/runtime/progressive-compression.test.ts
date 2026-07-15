import { createRunner } from "./helpers.js"
import { collectText } from "../../src/runtime/runner.js"
import { tool } from "../../src/tools/index.js"
import type { LLMProvider, RenderedContext, StreamEvent, ToolSchema, Message } from "../../src/types.js"

// Captures what the provider sees in context on each call
function trackingProvider(
  decide: (context: RenderedContext, callCount: number) => AsyncIterable<StreamEvent>,
): LLMProvider & { calls: RenderedContext[] } {
  const calls: RenderedContext[] = []
  return {
    calls,
    async complete(_ctx: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
      calls.push(context)
      yield* decide(context, calls.length)
    },
  }
}

function hasCompressed(events: Array<{ event: { kind: string } }>): boolean {
  return events.some(e => e.event.kind === "compressed")
}

// SnipCompact: oversized messages get head…[omitted]…tail treatment.
// After snip, the head and tail of the original content must still be visible.
describe("SnipCompact — head/tail preserved after truncation", () => {
  it("provider still sees head and tail of snipped tool result", async () => {
    const MARKER_HEAD = "CRITICAL_START_MARKER"
    const MARKER_TAIL = "CRITICAL_END_MARKER"

    let seenHead = false
    let seenTail = false

    const provider = trackingProvider(async function* (context, n) {
      // tool results live in contentParts[].output, not turn.content
      const allText = context.turns.flatMap(t => [
        t.content ?? "",
        ...(t.contentParts ?? []).map((p: any) => p.output ?? ""),
      ]).join(" ")
      if (allText.includes(MARKER_HEAD)) seenHead = true
      if (allText.includes(MARKER_TAIL)) seenTail = true

      if (n > 8) {
        yield { type: "text_delta" as const, delta: "done" }
        return
      }
      yield { type: "tool_call" as const, id: `c${n}`, name: "fetch", arguments: { q: "x" } }
    })

    const { runner, sessionLog } = createRunner(
      provider,
      [
        tool("fetch", "fetch", { type: "object", properties: { q: { type: "string" } } }, () =>
          // Large output: critical markers at head and tail, padding in middle
          `${MARKER_HEAD} ${"x".repeat(1000)} ${MARKER_TAIL}`,
        ),
      ],
      { maxTokens: 1024, maxTurns: 20 },
    )

    await collectText(runner.run({ sessionId: "snip-test", goal: "fetch then verify" }))

    expect(hasCompressed(await sessionLog.read("snip-test") as any)).toBe(true)
    // After snip, head and tail markers must still be visible in context
    expect(seenHead).toBe(true)
    expect(seenTail).toBe(true)
  })
})

// MicroCompact: tool results in preserved_refs must survive intact.
// We can't set preserved_refs directly from the Node SDK, so we verify the
// inverse: a non-preserved large tool result gets replaced with a placeholder
// that still contains the call_id and token count — enough for the agent to
// know what happened.
describe("MicroCompact — tool result placeholder retains call_id", () => {
  it("provider sees call_id in placeholder after micro-compact", async () => {
    let placeholderSeen = false

    const provider = trackingProvider(async function* (context, n) {
      const allText = context.turns.flatMap(t => [
        t.content ?? "",
        ...(t.contentParts ?? []).map((p: any) => p.output ?? ""),
      ]).join(" ")
      if (allText.includes("[tool result: call_big")) placeholderSeen = true

      if (n > 8) {
        yield { type: "text_delta" as const, delta: "done" }
        return
      }
      yield { type: "tool_call" as const, id: `call_big${n}`, name: "heavy", arguments: {} }
    })

    const { runner, sessionLog } = createRunner(
      provider,
      [
        // ~900 tokens per result — above micro_compact's 200-token threshold
        tool("heavy", "heavy", { type: "object", properties: {} }, () => "y".repeat(3600)),
      ],
      // Large enough budget that snip/micro fire before collapse/auto. The script deliberately
      // repeats an identical `heavy()` call every turn to build up rho — incidental to the repeat
      // fuse's intent, so it's disabled for this test.
      { maxTokens: 8192, maxTurns: 20, repeatFuse: false },
    )

    await collectText(runner.run({ sessionId: "micro-test", goal: "heavy then verify" }))

    expect(hasCompressed(await sessionLog.read("micro-test") as any)).toBe(true)
    expect(placeholderSeen).toBe(true)
  })
})

// ContextCollapse: oldest messages are dropped, a RuleSummarizer summary is
// prepended. The summary contains tool names and last assistant output.
// After collapse, the agent must be able to read the summary and complete a
// follow-up task that depends on knowing what tools were used.
describe("ContextCollapse — summary contains tool names for follow-up task", () => {
  it("provider sees tool name in summary after context collapse", async () => {
    let summaryWithToolName = false
    let callCount = 0

    const provider = trackingProvider(async function* (context, n) {
      callCount = n
      const allText = context.turns.map(t => t.content ?? "").join(" ")

      // RuleSummarizer six-slot format renders tool calls under `artifacts:` as
      // "- tool <name> args <args>". CollapseCompactor routes the summary through
      // compression_log → task_state → systemText/stateTurn.
      const allCtx = [context.systemText, context.systemStable, context.stateTurn?.content, context.turns[0]?.content].filter(Boolean).join("\n")
      if (allCtx.includes("[Compressed: context_collapse]") && allCtx.includes("tool accumulate")) {
        summaryWithToolName = true
        yield { type: "text_delta" as const, delta: "done" }
        return
      }

      if (n > 25) {
        yield { type: "text_delta" as const, delta: "done" }
        return
      }

      yield { type: "usage" as const, totalTokens: 941, inputTokens: 940, outputTokens: 1 }
      yield { type: "tool_call" as const, id: `c${n}`, name: "accumulate", arguments: { step: n } }
    })

    const { runner, sessionLog } = createRunner(
      provider,
      [
        tool("accumulate", "accumulate", { type: "object", properties: { step: { type: "number" } } }, () =>
          "z ".repeat(200),
        ),
      ],
      { maxTokens: 1024, maxTurns: 40 },
    )

    await collectText(runner.run({ sessionId: "collapse-test", goal: "accumulate then verify" }))

    const events = await sessionLog.read("collapse-test") as any
    const collapseEvents = events.filter(
      (e: any) => e.event.kind === "compressed" && e.event.action === "context_collapse",
    )
    expect(collapseEvents.length).toBeGreaterThan(0)
    expect(summaryWithToolName).toBe(true)
  })
})

// AutoCompact: full history collapsed, summary injected into working partition.
// After auto-compact, the provider must see the summary in context (as a system
// message) and be able to complete a task that references it.
describe("AutoCompact — summary injected into working partition", () => {
  it("provider sees compressed summary in context after auto-compact", async () => {
    let sawAutoCompactSummary = false

    const provider = trackingProvider(async function* (context, n) {
      const allText = context.turns.map(t => t.content ?? "").join(" ")

      // AutoCompact summary routes through compression_log → task_state → systemText/stateTurn
      const allCtx = [context.systemText, context.systemStable, context.stateTurn?.content, context.turns[0]?.content].filter(Boolean).join("\n")
      if (allCtx.includes("[Compressed: auto_compact]")) {
        sawAutoCompactSummary = true
        yield { type: "text_delta" as const, delta: "done" }
        return
      }

      if (n > 40) {
        yield { type: "text_delta" as const, delta: "done" }
        return
      }

      yield { type: "usage" as const, totalTokens: 1001, inputTokens: 1000, outputTokens: 1 }
      yield { type: "tool_call" as const, id: `c${n}`, name: "fill", arguments: { n } }
    })

    const { runner, sessionLog } = createRunner(
      provider,
      [
        tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () =>
          "w ".repeat(200),
        ),
      ],
      // Very tight budget to force auto_compact
      { maxTokens: 1024, maxTurns: 60 },
    )

    await collectText(runner.run({ sessionId: "auto-test", goal: "fill then verify" }))

    const events = await sessionLog.read("auto-test") as any
    const autoEvents = events.filter(
      (e: any) => e.event.kind === "compressed" && e.event.action === "auto_compact",
    )
    expect(autoEvents.length).toBeGreaterThan(0)
    expect(sawAutoCompactSummary).toBe(true)
  })
})

// Reactive compact: 413 triggers force_compact, retry succeeds.
describe("Reactive compact — 413 triggers force_compact and run recovers", () => {
  it("recovers after 413 and completes task", async () => {
    let callCount = 0
    const provider: LLMProvider = {
      async complete(_ctx: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(_context: RenderedContext): AsyncIterable<StreamEvent> {
        callCount += 1
        if (callCount === 1) throw new Error("413 context length exceeded")
        yield { type: "text_delta" as const, delta: "recovered" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("noop", "noop", { type: "object", properties: {} }, () => "ok")],
      {
        maxTokens: 4096,
        maxTurns: 5,
        compressionStore: {
          async write(sessionId, seq) { return `${sessionId}@${seq}` },
          async read() { return [] },
        },
      },
    )

    const sid = "reactive-test"
    await sessionLog.append(sid, { kind: "run_started", run_id: "r1", goal: "test", criteria: [] })
    // Seed valid, complete tool transactions so ContextUnit can archive old units without ever
    // splitting a call/result pair. Standalone assistant rows would form one degenerate unit and
    // are not representative of a replayable provider transcript.
    for (let i = 0; i < 6; i++) {
      const callId = `seed-${i}`
      await sessionLog.append(sid, {
        kind: "llm_completed",
        turn: i,
        content: "seed",
        tool_calls: [{ id: callId, name: "noop", arguments: "{}" }],
      })
      await sessionLog.append(sid, {
        kind: "tool_completed",
        turn: i,
        results: [{ call_id: callId, output: "x ".repeat(200) }],
      })
    }
    // Leave a pending tool call so wake() re-enters the provider
    await sessionLog.append(sid, { kind: "llm_completed", turn: 6, content: "pending", tool_calls: [{ id: "cpending", name: "noop", arguments: "{}" }] })

    const text = await collectText(runner.wake(sid))
    expect(text).toBe("recovered")
    expect(hasCompressed(await sessionLog.read(sid) as any)).toBe(true)
  })
})
