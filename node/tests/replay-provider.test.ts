import { ReplayProvider } from "../src/runtime/replay-provider.js"
import { extractRecordedMessages } from "../src/runtime/replay-fixture.js"
import type { Message, RenderedContext, ToolSchema, UsageEvent, TextDelta, ToolCallEvent } from "../src/types.js"
import type { SessionEvent } from "../src/runtime/session-log.js"

// ── helpers ─────────────────────────────────────────────────────────────────

const emptyCtx = (): RenderedContext => ({ systemText: "", turns: [] })
const noTools: ToolSchema[] = []

async function collect(provider: ReplayProvider, ctx: RenderedContext = emptyCtx(), tools: ToolSchema[] = noTools) {
  const events: Array<UsageEvent | TextDelta | ToolCallEvent> = []
  for await (const evt of provider.stream(ctx, tools)) {
    events.push(evt as never)
  }
  return events
}

// ── unit tests ──────────────────────────────────────────────────────────────

describe("ReplayProvider", () => {
  it("emits usage + text_delta + tool_call for a recorded message", async () => {
    const msg: Message = {
      role: "assistant",
      content: "I will call read_file.",
      toolCalls: [{ id: "c1", name: "read_file", arguments: JSON.stringify({ path: "src/x.ts" }) }],
      tokenCount: 42,
    }
    const provider = new ReplayProvider([msg])
    const events = await collect(provider)

    expect(events.map(e => e.type)).toEqual(["usage", "text_delta", "tool_call"])

    const usage = events[0] as UsageEvent
    expect(usage.outputTokens).toBe(42)
    expect(usage.inputTokens).toBe(0) // empty ctx + no tools → 0 tokens
    expect(usage.cacheReadInputTokens).toBe(0)

    const delta = events[1] as TextDelta
    expect(delta.delta).toBe("I will call read_file.")

    const call = events[2] as ToolCallEvent
    expect(call.id).toBe("c1")
    expect(call.name).toBe("read_file")
    expect(call.arguments).toEqual({ path: "src/x.ts" })
  })

  it("estimates inputTokens from the rendered context this call carries", async () => {
    const longSystem = "x".repeat(800) // 800 chars / 4 = 200 tokens
    const ctx: RenderedContext = { systemText: longSystem, turns: [] }
    const provider = new ReplayProvider([{ role: "assistant", content: "ok" }])
    const events = await collect(provider, ctx)
    const usage = events[0] as UsageEvent
    expect(usage.inputTokens).toBe(200)
  })

  it("respects a custom tokenizer", async () => {
    const provider = new ReplayProvider([{ role: "assistant", content: "ok" }], {
      tokenizer: text => text.length, // 1 token per char
    })
    const ctx: RenderedContext = { systemText: "abcdef", turns: [] }
    const events = await collect(provider, ctx)
    const usage = events[0] as UsageEvent
    expect(usage.inputTokens).toBe(6)
  })

  it("advances cursor across calls", async () => {
    const msgs: Message[] = [
      { role: "assistant", content: "first" },
      { role: "assistant", content: "second" },
      { role: "assistant", content: "third" },
    ]
    const provider = new ReplayProvider(msgs)
    expect(provider.consumed()).toBe(0)
    expect(provider.remaining()).toBe(3)

    const evs1 = await collect(provider)
    expect((evs1[1] as TextDelta).delta).toBe("first")
    expect(provider.consumed()).toBe(1)

    const evs2 = await collect(provider)
    expect((evs2[1] as TextDelta).delta).toBe("second")

    const evs3 = await collect(provider)
    expect((evs3[1] as TextDelta).delta).toBe("third")
    expect(provider.remaining()).toBe(0)
  })

  it("throws when fixture is exhausted (default behavior)", async () => {
    const provider = new ReplayProvider([{ role: "assistant", content: "only" }])
    await collect(provider) // consume
    await expect(collect(provider)).rejects.toThrow(/fixture exhausted/)
  })

  it("wraps around when wrap=true", async () => {
    const provider = new ReplayProvider(
      [
        { role: "assistant", content: "a" },
        { role: "assistant", content: "b" },
      ],
      { wrap: true },
    )
    const seq: string[] = []
    for (let i = 0; i < 5; i++) {
      const events = await collect(provider)
      seq.push((events[1] as TextDelta).delta)
    }
    expect(seq).toEqual(["a", "b", "a", "b", "a"])
  })

  it("reset() rewinds to the start", async () => {
    const provider = new ReplayProvider([
      { role: "assistant", content: "one" },
      { role: "assistant", content: "two" },
    ])
    await collect(provider)
    await collect(provider)
    expect(provider.remaining()).toBe(0)
    provider.reset()
    expect(provider.remaining()).toBe(2)
    const events = await collect(provider)
    expect((events[1] as TextDelta).delta).toBe("one")
  })

  it("complete() returns the same Message that stream() would emit", async () => {
    const msg: Message = {
      role: "assistant",
      content: "answer",
      toolCalls: [{ id: "c", name: "noop", arguments: "{}" }],
    }
    const provider = new ReplayProvider([msg, msg])
    const out = await provider.complete(emptyCtx(), noTools)
    expect(out.role).toBe("assistant")
    expect(out.content).toBe("answer")
    expect(out.toolCalls).toEqual([{ id: "c", name: "noop", arguments: "{}" }])
  })

  it("tolerates malformed recorded arguments (passes {})", async () => {
    const provider = new ReplayProvider([
      {
        role: "assistant",
        content: "",
        toolCalls: [{ id: "x", name: "buggy", arguments: "not-json{" }],
      },
    ])
    const events = await collect(provider)
    const call = events.find(e => e.type === "tool_call") as ToolCallEvent
    expect(call.arguments).toEqual({})
  })

  it("descriptor() returns the default replay descriptor when none provided", () => {
    const provider = new ReplayProvider([])
    expect(provider.descriptor()).toMatchObject({ provider: "replay", protocol: "openai-chat" })
  })
})

// ── fixture extraction ──────────────────────────────────────────────────────

describe("extractRecordedMessages", () => {
  it("pulls llm_completed events in order", () => {
    const events: Array<{ event: SessionEvent }> = [
      { event: { kind: "run_started" } as unknown as SessionEvent },
      {
        event: {
          kind: "llm_completed",
          turn: 1,
          content: "hello",
          tokenCount: 5,
          toolCalls: [{ id: "c1", name: "tool_a", arguments: '{"a":1}' }],
        } as unknown as SessionEvent,
      },
      { event: { kind: "tool_requested" } as unknown as SessionEvent },
      {
        event: {
          kind: "llm_completed",
          turn: 2,
          content: "follow-up",
          toolCalls: [],
        } as unknown as SessionEvent,
      },
    ]
    const messages = extractRecordedMessages(events)
    expect(messages.length).toBe(2)
    expect(messages[0].content).toBe("hello")
    expect(messages[0].toolCalls?.[0].name).toBe("tool_a")
    expect(messages[0].tokenCount).toBe(5)
    expect(messages[1].content).toBe("follow-up")
    expect(messages[1].toolCalls).toBeUndefined()
  })

  it("accepts a bare SessionEvent[] (unwrapped)", () => {
    const events: SessionEvent[] = [
      { kind: "llm_completed", turn: 1, content: "a" } as unknown as SessionEvent,
      { kind: "llm_completed", turn: 2, content: "b" } as unknown as SessionEvent,
    ]
    const messages = extractRecordedMessages(events)
    expect(messages.map(m => m.content)).toEqual(["a", "b"])
  })

  it("accepts snake_case fields from serialised session logs (tool_calls / token_count)", () => {
    // The session-log on-disk shape uses snake_case (tool_calls / token_count / provider_replay).
    // This is a regression test for a benchmark-replay-mode bug where extractRecordedMessages was
    // reading only camelCase, causing replay to think every recorded turn had no tool calls.
    const events: Array<{ event: SessionEvent }> = [
      {
        event: {
          kind: "llm_completed",
          turn: 0,
          content: "calling skill",
          tool_calls: [{ id: "c0", name: "skill", arguments: '{"name":"debugging"}' }],
          token_count: 12,
        } as unknown as SessionEvent,
      },
      {
        event: {
          kind: "llm_completed",
          turn: 1,
          content: "reading files",
          tool_calls: [
            { id: "c1", name: "read_file", arguments: '{"path":"src/x.ts"}' },
            { id: "c2", name: "read_file", arguments: '{"path":"src/y.ts"}' },
          ],
          token_count: 30,
        } as unknown as SessionEvent,
      },
    ]
    const messages = extractRecordedMessages(events)
    expect(messages.length).toBe(2)
    expect(messages[0].toolCalls?.length).toBe(1)
    expect(messages[0].toolCalls?.[0].name).toBe("skill")
    expect(messages[0].tokenCount).toBe(12)
    expect(messages[1].toolCalls?.length).toBe(2)
    expect(messages[1].tokenCount).toBe(30)
  })

  it("normalises non-string tool-call arguments to JSON strings", () => {
    const events: Array<{ event: SessionEvent }> = [
      {
        event: {
          kind: "llm_completed",
          content: "",
          toolCalls: [{ id: "c", name: "noop", arguments: { foo: "bar" } as unknown as string }],
        } as unknown as SessionEvent,
      },
    ]
    const messages = extractRecordedMessages(events)
    expect(messages[0].toolCalls?.[0].arguments).toBe('{"foo":"bar"}')
  })
})

// ── round-trip: record → replay yields identical output sequence ────────────

describe("ReplayProvider round-trip", () => {
  it("returns the recorded text/toolCalls across two independent replays", async () => {
    const msgs: Message[] = [
      { role: "assistant", content: "step 1", toolCalls: [{ id: "1", name: "a", arguments: "{}" }] },
      { role: "assistant", content: "step 2", toolCalls: [{ id: "2", name: "b", arguments: '{"x":1}' }] },
      { role: "assistant", content: "done" },
    ]

    const collectAll = async (p: ReplayProvider) => {
      const seen: Array<{ text: string; calls: string[] }> = []
      for (let i = 0; i < msgs.length; i++) {
        const evs = await collect(p)
        const text = (evs.find(e => e.type === "text_delta") as TextDelta | undefined)?.delta ?? ""
        const calls = evs.filter(e => e.type === "tool_call").map(e => (e as ToolCallEvent).name)
        seen.push({ text, calls })
      }
      return seen
    }

    const a = await collectAll(new ReplayProvider(msgs))
    const b = await collectAll(new ReplayProvider(msgs))
    expect(a).toEqual(b)
    expect(a).toEqual([
      { text: "step 1", calls: ["a"] },
      { text: "step 2", calls: ["b"] },
      { text: "done", calls: [] },
    ])
  })
})
