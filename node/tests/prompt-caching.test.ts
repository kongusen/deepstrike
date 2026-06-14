import { AnthropicProvider } from "../src/providers/anthropic.js"
import { cacheHitRate } from "../src/providers/base.js"
import type { RenderedContext, UsageEvent } from "../src/types.js"

/** Count every cache_control breakpoint across system + tools + messages. */
function countBreakpoints(params: any): number {
  let n = 0
  const countBlocks = (content: unknown) => {
    if (Array.isArray(content)) {
      for (const block of content) {
        if (block && typeof block === "object" && "cache_control" in block) n++
      }
    }
  }
  countBlocks(params.system)
  if (Array.isArray(params.tools)) {
    for (const t of params.tools) if (t && "cache_control" in t) n++
  }
  if (Array.isArray(params.messages)) {
    for (const m of params.messages) countBlocks(m.content)
  }
  return n
}

function stubProvider(): { provider: AnthropicProvider; captured: () => any } {
  const provider = new AnthropicProvider("test-key")
  let capturedParams: any = null
  ;(provider as any).client = {
    messages: {
      create: async (params: any) => {
        capturedParams = params
        return {
          content: [{ type: "text", text: "hello" }],
          usage: { input_tokens: 100, output_tokens: 20 },
        }
      },
    },
  }
  return { provider, captured: () => capturedParams }
}

describe("Anthropic Prompt Caching", () => {
  it("caches system blocks and rolls breakpoints across the message tail", async () => {
    const { provider, captured } = stubProvider()

    const context: RenderedContext = {
      systemText: "system rules\nskill: debug",
      systemStable: "system rules",
      systemKnowledge: "skill: debug",
      turns: [
        { role: "user", content: "[TASK STATE] goal: do it\n\nProceed." },
        { role: "assistant", content: "first assistant message" },
        { role: "user", content: "second user message" },
      ],
    }
    const tools = [
      { name: "tool1", description: "first tool", parameters: "{}" },
      { name: "tool2", description: "second tool", parameters: "{}" },
    ]

    await provider.complete(context, tools)
    const params = captured()
    expect(params).toBeDefined()

    // systemStable + systemKnowledge stay as separate cache blocks.
    expect(params.system).toEqual([
      { type: "text", text: "system rules", cache_control: { type: "ephemeral" } },
      { type: "text", text: "skill: debug", cache_control: { type: "ephemeral" } },
    ])

    // The tool breakpoint is dropped: the systemStable block already caches the
    // tools prefix, freeing slots for the message history.
    expect(params.tools[0].cache_control).toBeUndefined()
    expect(params.tools[1].cache_control).toBeUndefined()

    // The last message and the nearest preceding user turn carry breakpoints;
    // a bare string body is promoted to a cache-bearing text block.
    expect(params.messages).toHaveLength(3)
    expect(params.messages[0].content).toEqual([
      { type: "text", text: "[TASK STATE] goal: do it\n\nProceed.", cache_control: { type: "ephemeral" } },
    ])
    expect(params.messages[1]).toEqual({ role: "assistant", content: "first assistant message" })
    expect(params.messages[2].content).toEqual([
      { type: "text", text: "second user message", cache_control: { type: "ephemeral" } },
    ])

    // Never exceed Anthropic's 4-breakpoint budget.
    expect(countBreakpoints(params)).toBe(4)
  })

  it("handles empty systemKnowledge and a single message", async () => {
    const { provider, captured } = stubProvider()

    const context: RenderedContext = {
      systemText: "system rules",
      systemStable: "system rules",
      turns: [{ role: "user", content: "single message" }],
    }

    await provider.complete(context, [])
    const params = captured()

    expect(params.system).toEqual([
      { type: "text", text: "system rules", cache_control: { type: "ephemeral" } },
    ])
    expect(params.messages[0].content).toEqual([
      { type: "text", text: "single message", cache_control: { type: "ephemeral" } },
    ])
    expect(countBreakpoints(params)).toBeLessThanOrEqual(4)
  })

  it("anchors the tool breakpoint when system is an unpartitioned string", async () => {
    const { provider, captured } = stubProvider()

    const context: RenderedContext = {
      systemText: "flat system prompt",
      turns: [
        { role: "user", content: "hello" },
        { role: "assistant", content: "hi" },
        { role: "user", content: "again" },
      ],
    }
    const tools = [
      { name: "a", description: "a", parameters: "{}" },
      { name: "b", description: "b", parameters: "{}" },
    ]

    await provider.complete(context, tools)
    const params = captured()

    // No structured system blocks -> system stays a plain string, so the final
    // tool anchors the static prefix cache.
    expect(params.system).toBe("flat system prompt")
    expect(params.tools[0].cache_control).toBeUndefined()
    expect(params.tools[1].cache_control).toEqual({ type: "ephemeral" })

    // Messages still get rolling breakpoints. Total = 1 tool + 2 messages = 3.
    expect(countBreakpoints(params)).toBe(3)
  })

  it("renders stateTurn after the cached history with no breakpoint on it", async () => {
    const { provider, captured } = stubProvider()

    const context: RenderedContext = {
      systemText: "rules",
      systemStable: "rules",
      turns: [
        { role: "user", content: "earlier question" },
        { role: "assistant", content: "earlier answer" },
      ],
      stateTurn: { role: "user", content: "[TASK STATE] goal: g\n\nProceed." },
    }

    await provider.complete(context, [])
    const params = captured()

    // history (2) + state (1) appended last
    expect(params.messages).toHaveLength(3)
    // the state turn is the uncached tail (plain string content, no cache_control)
    expect(params.messages[2]).toEqual({ role: "user", content: "[TASK STATE] goal: g\n\nProceed." })
    // the history tail carries the rolling read-anchor breakpoint
    const hasCache = (c: unknown) => Array.isArray(c) && c.some(b => b && typeof b === "object" && "cache_control" in b)
    expect(hasCache(params.messages[0].content) || hasCache(params.messages[1].content)).toBe(true)
    expect(countBreakpoints(params)).toBeLessThanOrEqual(4)
  })

  it("never exceeds 4 breakpoints and marks only the last two user turns on long histories", async () => {
    const { provider, captured } = stubProvider()

    const context: RenderedContext = {
      systemText: "rules\nknowledge",
      systemStable: "rules",
      systemKnowledge: "knowledge",
      turns: [
        { role: "user", content: "task" },
        { role: "assistant", content: "", toolCalls: [{ id: "c1", name: "t", arguments: "{}" }] },
        { role: "tool", contentParts: [{ type: "tool_result", callId: "c1", output: "r1", isError: false }] },
        { role: "assistant", content: "", toolCalls: [{ id: "c2", name: "t", arguments: "{}" }] },
        { role: "tool", contentParts: [{ type: "tool_result", callId: "c2", output: "r2", isError: false }] },
      ],
    }
    const tools = [{ name: "t", description: "t", parameters: "{}" }]

    await provider.complete(context, tools)
    const params = captured()

    // 2 system + 2 message = 4; tool breakpoint dropped.
    expect(countBreakpoints(params)).toBe(4)

    const hasCache = (content: unknown) =>
      Array.isArray(content) && content.some(b => b && typeof b === "object" && "cache_control" in b)

    // wire messages: [0]user task, [1]assistant, [2]user tool_result,
    // [3]assistant, [4]user tool_result. Only the last two user turns (2 and 4).
    expect(hasCache(params.messages[0].content)).toBe(false)
    expect(hasCache(params.messages[1].content)).toBe(false)
    expect(hasCache(params.messages[2].content)).toBe(true)
    expect(hasCache(params.messages[3].content)).toBe(false)
    expect(hasCache(params.messages[4].content)).toBe(true)
  })

  // ── P0-A: cacheable prefix stays byte-stable as the session grows ──────────

  it("keeps the cacheable prefix byte-stable as history grows across turns", async () => {
    const { provider, captured } = stubProvider()

    // Token-level content of a wire message, ignoring cache_control markers and the
    // string↔single-text-block promotion (both tokenize identically, so neither
    // affects cache matching). This is what must NOT drift across turns.
    const textOf = (msg: any): string => {
      const c = msg.content
      if (typeof c === "string") return c
      if (Array.isArray(c)) {
        return c
          .map(b => (b.type === "text" ? b.text : JSON.stringify({ ...b, cache_control: undefined })))
          .join(" ")
      }
      return JSON.stringify(c)
    }

    const baseTurns: RenderedContext["turns"] = [
      { role: "user", content: "turn 1" },
      { role: "assistant", content: "answer 1" },
    ]
    const ctx1: RenderedContext = {
      systemText: "rules\nkb", systemStable: "rules", systemKnowledge: "kb",
      turns: [...baseTurns],
      stateTurn: { role: "user", content: "[TASK STATE] goal: a\n\nProceed." },
    }
    await provider.complete(ctx1, [])
    const p1 = captured()
    const prefix1 = [p1.messages[0], p1.messages[1]].map(textOf)

    // Next turn: two more history turns appended AND a different volatile state.
    const ctx2: RenderedContext = {
      ...ctx1,
      turns: [...baseTurns, { role: "user", content: "turn 2" }, { role: "assistant", content: "answer 2" }],
      stateTurn: { role: "user", content: "[TASK STATE] goal: b (changed)\n\nProceed." },
    }
    await provider.complete(ctx2, [])
    const p2 = captured()
    const prefix2 = [p2.messages[0], p2.messages[1]].map(textOf)

    // The shared history prefix is byte-identical — only the tail grew, and the
    // volatile state lives in the uncached tail, so it never perturbs the prefix.
    expect(prefix2).toEqual(prefix1)
    // System blocks are untouched, and the budget invariant holds on both turns.
    expect(p2.system).toEqual(p1.system)
    expect(countBreakpoints(p1)).toBeLessThanOrEqual(4)
    expect(countBreakpoints(p2)).toBeLessThanOrEqual(4)
  })

  // ── P1-E: two-tier breakpoint (deep frozen anchor + rolling tail) ──────────

  it("pins a deep breakpoint at the frozen boundary and rolls the tail", async () => {
    const { provider, captured } = stubProvider()

    // 5 history turns; frozen prefix = first 2 (the compaction boundary).
    const context: RenderedContext = {
      systemText: "rules\nkb", systemStable: "rules", systemKnowledge: "kb",
      turns: [
        { role: "user", content: "t0 (frozen)" },
        { role: "assistant", content: "t1 (frozen)" },
        { role: "user", content: "t2 hot" },
        { role: "assistant", content: "t3 hot" },
        { role: "user", content: "t4 hot tail" },
      ],
      frozenPrefixLen: 2,
    }
    await provider.complete(context, [])
    const params = captured()

    const hasCache = (c: unknown) =>
      Array.isArray(c) && c.some(b => b && typeof b === "object" && "cache_control" in b)

    // Deep breakpoint on the last frozen turn (index 1), rolling breakpoint on the final turn (index 4).
    expect(hasCache(params.messages[1].content)).toBe(true) // deep anchor (frozen boundary)
    expect(hasCache(params.messages[4].content)).toBe(true) // rolling tail
    // The intermediate hot turns and the very first turn carry no breakpoint.
    expect(hasCache(params.messages[0].content)).toBe(false)
    expect(hasCache(params.messages[2].content)).toBe(false)
    expect(hasCache(params.messages[3].content)).toBe(false)
    // 2 system + 2 message = 4, never exceeding the budget.
    expect(countBreakpoints(params)).toBe(4)
  })

  it("falls back to the rolling pair when frozenPrefixLen is absent (dual-path)", async () => {
    const { provider, captured } = stubProvider()
    const context: RenderedContext = {
      systemText: "rules", systemStable: "rules",
      turns: [
        { role: "user", content: "a" },
        { role: "assistant", content: "b" },
        { role: "user", content: "c" },
      ],
      // no frozenPrefixLen — older binding / no compaction yet
    }
    await provider.complete(context, [])
    const params = captured()

    const hasCache = (c: unknown) =>
      Array.isArray(c) && c.some(b => b && typeof b === "object" && "cache_control" in b)
    // Rolling pair: final message + nearest preceding user turn (index 0).
    expect(hasCache(params.messages[2].content)).toBe(true)
    expect(hasCache(params.messages[0].content)).toBe(true)
    expect(hasCache(params.messages[1].content)).toBe(false)
    expect(countBreakpoints(params)).toBeLessThanOrEqual(4)
  })

  it("renders tool definitions byte-identically across turns (B4 stability guard)", async () => {
    const { provider, captured } = stubProvider()
    const tools = [
      { name: "alpha", description: "a", parameters: JSON.stringify({ type: "object", properties: { x: { type: "string" } } }) },
      { name: "beta", description: "b", parameters: "{}" },
    ]
    const ctx = (n: number): RenderedContext => ({
      systemText: "rules", systemStable: "rules",
      turns: Array.from({ length: n }, (_, i) => ({ role: i % 2 ? "assistant" : "user", content: `m${i}` }) as const),
    })

    await provider.complete(ctx(2), tools)
    const t1 = captured().tools
    await provider.complete(ctx(6), tools)
    const t2 = captured().tools

    // Tools render BEFORE system on the Anthropic wire, so any drift in tool bytes
    // invalidates the entire prompt cache. The same tool set must serialize
    // byte-identically regardless of how deep the conversation has grown.
    expect(t2).toEqual(t1)
  })

  it("reports cache read/creation tokens and a hit rate from streamed usage", async () => {
    const provider = new AnthropicProvider("test-key")
    ;(provider as any).client = {
      messages: {
        stream: () => ({
          async *[Symbol.asyncIterator]() {
            yield {
              type: "message_start",
              message: {
                usage: {
                  input_tokens: 200, // uncached
                  cache_read_input_tokens: 1600,
                  cache_creation_input_tokens: 200,
                  output_tokens: 0,
                },
              },
            }
            yield { type: "message_delta", usage: { output_tokens: 50 } }
          },
        }),
      },
    }

    let usage: UsageEvent | undefined
    for await (const evt of provider.stream({ systemText: "rules", turns: [{ role: "user", content: "hi" }] }, [])) {
      if (evt.type === "usage") usage = evt as UsageEvent
    }

    expect(usage).toBeDefined()
    // inputTokens is the FULL prompt: uncached + cache read + cache write.
    expect(usage!.inputTokens).toBe(2000)
    expect(usage!.cacheReadInputTokens).toBe(1600)
    expect(usage!.cacheCreationInputTokens).toBe(200)
    // 1600 / 2000 = 0.8 of the prompt served from cache.
    expect(cacheHitRate(usage!)).toBeCloseTo(0.8)
    expect(cacheHitRate({ inputTokens: 0 })).toBe(0)
  })
})
