/**
 * Server-side compaction beyond the plain threshold edit: a threshold compaction that pauses so
 * the newest history stays verbatim behind the summary, and on-demand compaction that the
 * provider requests in the background and swaps in for the messages it summarized.
 */
import { AnthropicProvider } from "../src/providers/anthropic.js"
import type { ModelMessage, RenderedContext } from "../src/types.js"

type Body = Record<string, any>
type Chunk = Record<string, unknown>

function textChunks(text: string, usage = { input_tokens: 10, output_tokens: 5 }): Chunk[] {
  return [
    { type: "message_start", message: { id: "msg_text", usage: { input_tokens: usage.input_tokens, output_tokens: 0 } } },
    { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } },
    { type: "content_block_stop", index: 0 },
    { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: usage.output_tokens } },
  ]
}

const PAUSE_CHUNKS: Chunk[] = [
  { type: "message_start", message: { id: "msg_pause", usage: { input_tokens: 0, output_tokens: 0 } } },
  { type: "content_block_start", index: 0, content_block: { type: "compaction", content: null } },
  { type: "content_block_delta", index: 0, delta: { type: "compaction_delta", content: "Summary so far." } },
  { type: "content_block_stop", index: 0 },
  { type: "message_delta", delta: { stop_reason: "compaction" }, usage: { output_tokens: 0 } },
]

/** Scripted streams, one per request, and the bodies they were sent. */
function scriptStreams(provider: AnthropicProvider, scripts: Chunk[][]): Body[] {
  const bodies: Body[] = []
  ;(provider as unknown as { streamMessage: (params: Body) => AsyncIterable<unknown> }).streamMessage = params => {
    bodies.push(structuredClone(params))
    const chunks = scripts[bodies.length - 1] ?? []
    return (async function*() { for (const chunk of chunks) yield chunk })()
  }
  return bodies
}

function scriptCreates(provider: AnthropicProvider, responses: Body[]): Body[] {
  const bodies: Body[] = []
  ;(provider as unknown as { createMessage: (params: Body) => Promise<Body> }).createMessage = async params => {
    bodies.push(structuredClone(params))
    const response = responses[bodies.length - 1]
    if (!response) throw new Error("no scripted response")
    return response
  }
  return bodies
}

async function drain(provider: AnthropicProvider, context: RenderedContext): Promise<string> {
  let text = ""
  for await (const event of provider.stream(context, [])) if (event.type === "text_delta") text += event.delta
  return text
}

function ctx(turns: ModelMessage[]): RenderedContext {
  return { systemText: "stable", systemStable: "stable", turns }
}

describe("threshold compaction with a pause", () => {
  it("sends pause_after_compaction and rejects settings that do not fit the mode", () => {
    expect(() => new AnthropicProvider({
      apiKey: "sk-fake", compaction: { pauseAfterCompaction: { keepRecentMessages: -1 } },
    })).toThrow(/keepRecentMessages/)
    expect(() => new AnthropicProvider({
      apiKey: "sk-fake", compaction: { mode: "on_demand", compactAtInputTokens: 0 },
    })).toThrow(/compactAtInputTokens/)
    expect(() => new AnthropicProvider({
      apiKey: "sk-fake",
      compaction: { mode: "on_demand", compactAtInputTokens: 10, triggerInputTokens: 60_000 } as never,
    })).toThrow(/threshold/)
  })

  it("continues a paused turn with the block appended, then replays it ahead of the reply", async () => {
    const provider = new AnthropicProvider({
      apiKey: "sk-fake", model: "claude-opus-5-5",
      compaction: { triggerInputTokens: 60_000, pauseAfterCompaction: { keepRecentMessages: 0 } },
    })
    const bodies = scriptStreams(provider, [PAUSE_CHUNKS, textChunks("Continued.")])
    const first = ctx([{ role: "user", content: "u1" }])
    expect(await drain(provider, first)).toBe("Continued.")

    expect(bodies[0].context_management.edits).toEqual([{
      type: "compact_20260112", trigger: { type: "input_tokens", value: 60_000 }, pause_after_compaction: true,
    }])
    expect(bodies[1].context_management.edits).toEqual([{
      type: "compact_20260112", trigger: { type: "input_tokens", value: 60_000 },
    }])
    expect(bodies[1].messages.at(-1)).toEqual({
      role: "assistant", content: [{ type: "compaction", content: "Summary so far." }],
    })

    scriptStreams(provider, [textChunks("ok")])
    const next = ctx([...first.turns, { role: "assistant", content: "Continued." }, { role: "user", content: "u2" }])
    const nextBodies = scriptStreams(provider, [textChunks("ok")])
    await drain(provider, next)
    // Everything before the block is ignored by the API, so it is not sent.
    expect(nextBodies[0].messages[0]).toEqual({
      role: "assistant",
      content: [{ type: "compaction", content: "Summary so far." }, { type: "text", text: "Continued." }],
    })
    expect(nextBodies[0].messages).toHaveLength(2)
  })

  it("keeps the newest history verbatim behind the block, without its thinking", async () => {
    const provider = new AnthropicProvider({
      apiKey: "sk-fake", model: "claude-opus-5-5",
      compaction: { pauseAfterCompaction: { keepRecentMessages: 1 } },
    })
    const call = { id: "t1", name: "look", arguments: "{}" }
    const toolTurn: ModelMessage = { role: "assistant", content: "", toolCalls: [call] }
    provider.seedProviderReplay(toolTurn, {
      protocol: "anthropic-messages",
      native_blocks: [
        { type: "thinking", thinking: "", signature: "sig" },
        { type: "tool_use", id: "t1", name: "look", input: {} },
      ],
    })
    const turns: ModelMessage[] = [
      { role: "user", content: "u1" },
      { role: "assistant", content: "a1" },
      { role: "user", content: "u2" },
      toolTurn,
      { role: "tool", content: "seen", contentParts: [{ type: "tool_result", callId: "t1", output: "seen" }] } as ModelMessage,
    ]
    const bodies = scriptStreams(provider, [PAUSE_CHUNKS, textChunks("Done.")])
    expect(await drain(provider, ctx(turns))).toBe("Done.")

    const kept = [
      {
        role: "assistant",
        content: [
          { type: "compaction", content: "Summary so far." },
          { type: "tool_use", id: "t1", name: "look", input: {} },
        ],
      },
      expect.objectContaining({ role: "user" }),
    ]
    expect(bodies[1].messages).toEqual(kept)

    // The next request renders the same kept turns: the anchor turn's replay was rewritten.
    const nextBodies = scriptStreams(provider, [textChunks("ok")])
    await drain(provider, ctx([...turns, { role: "assistant", content: "Done." }, { role: "user", content: "u3" }]))
    expect(nextBodies[0].messages.slice(0, 2)).toEqual(kept)
    expect(nextBodies[0].messages).toHaveLength(4)
  })

  it("passes an ordinary response through untouched when the pause is configured", async () => {
    const provider = new AnthropicProvider({
      apiKey: "sk-fake", model: "claude-opus-5-5", compaction: { pauseAfterCompaction: { keepRecentMessages: 2 } },
    })
    const bodies = scriptStreams(provider, [textChunks("plain")])
    expect(await drain(provider, ctx([{ role: "user", content: "u1" }]))).toBe("plain")
    expect(bodies).toHaveLength(1)
  })

  it("continues a paused non-streaming turn the same way", async () => {
    const provider = new AnthropicProvider({
      apiKey: "sk-fake", model: "claude-opus-5-5", compaction: { pauseAfterCompaction: { keepRecentMessages: 0 } },
    })
    const bodies = scriptCreates(provider, [
      { id: "m1", stop_reason: "compaction", content: [{ type: "compaction", content: "S." }], usage: { input_tokens: 0, output_tokens: 0 } },
      { id: "m2", stop_reason: "end_turn", content: [{ type: "text", text: "after" }], usage: { input_tokens: 3, output_tokens: 1 } },
    ])
    const message = await provider.complete(ctx([{ role: "user", content: "u1" }]), [])
    expect(message.content).toBe("after")
    expect(bodies[1].messages.at(-1)).toEqual({ role: "assistant", content: [{ type: "compaction", content: "S." }] })
    expect(provider.peekProviderReplay(message)?.native_blocks).toEqual([
      { type: "compaction", content: "S." },
      { type: "text", text: "after" },
    ])
  })
})

describe("on-demand compaction", () => {
  const block = { type: "compaction", content: "Summary of u1..a1.", signature: "sig-1" }

  function onDemand(extra: Record<string, unknown> = {}): AnthropicProvider {
    return new AnthropicProvider({
      apiKey: "sk-fake", model: "claude-opus-5-5",
      compaction: { mode: "on_demand", compactAtInputTokens: 100, ...extra } as never,
    })
  }

  it("declares compaction native and sends no edit on ordinary requests", async () => {
    const provider = onDemand()
    expect(provider.descriptor().nativeContext).toEqual({ compaction: true })
    const bodies = scriptStreams(provider, [textChunks("hi")])
    await drain(provider, ctx([{ role: "user", content: "u1" }]))
    expect(bodies[0].context_management).toBeUndefined()
    expect(bodies[0].betas).toBeUndefined()
  })

  it("summarizes the history a turn sent once it passes the limit, then swaps the block in", async () => {
    const provider = onDemand({ instructions: "Keep the open questions.", maxTokens: 4096 })
    const compactions = scriptCreates(provider, [
      { stop_reason: "compaction", content: [block], usage: { input_tokens: 0, output_tokens: 0 } },
    ])
    const history: ModelMessage[] = [{ role: "user", content: "u1" }, { role: "assistant", content: "a1" }, { role: "user", content: "u2" }]
    const turn = { ...ctx(history), stateTurn: { role: "user", content: "[TASK STATE] t" } as ModelMessage }
    scriptStreams(provider, [textChunks("a2", { input_tokens: 90, output_tokens: 20 })])
    await drain(provider, turn)
    await provider.compactionIdle()

    expect(compactions).toHaveLength(1)
    const request = compactions[0]
    expect(request.compaction).toEqual({ type: "summarize", instructions: "Keep the open questions." })
    expect(request.betas).toEqual(["compact-2026-09-04"])
    expect(request.max_tokens).toBe(4096)
    expect(request.stream).toBeUndefined()
    expect(request.context_management).toBeUndefined()
    // The volatile state turn is not history: the summary covers the history messages only.
    expect(request.messages.map((m: Body) => m.role)).toEqual(["user", "assistant", "user"])
    expect(JSON.stringify(request.messages)).not.toContain("TASK STATE")

    const next = { ...ctx([...history, { role: "assistant", content: "a2" }, { role: "user", content: "u3" }]), stateTurn: turn.stateTurn }
    const bodies = scriptStreams(provider, [textChunks("a3")])
    await drain(provider, next)
    expect(bodies[0].betas).toEqual(["compact-2026-09-04"])
    expect(bodies[0].messages[0]).toEqual({ role: "assistant", content: [block] })
    expect(bodies[0].messages.slice(1).map((m: Body) => m.role)).toEqual(["assistant", "user", "user"])
  })

  it("sends the whole history when it no longer starts with the summarized messages", async () => {
    const provider = onDemand()
    scriptCreates(provider, [{ stop_reason: "compaction", content: [block], usage: {} }])
    const history: ModelMessage[] = [{ role: "user", content: "u1" }]
    scriptStreams(provider, [textChunks("a1", { input_tokens: 200, output_tokens: 1 })])
    await drain(provider, ctx(history))
    await provider.compactionIdle()

    // A renewal rebuilt the history: the summary no longer stands for its front.
    const bodies = scriptStreams(provider, [textChunks("ok"), textChunks("ok")])
    await drain(provider, ctx([{ role: "user", content: "renewed goal" }]))
    expect(bodies[0].messages).toEqual([expect.objectContaining({ role: "user" })])
    expect(bodies[0].betas).toBeUndefined()
    // The stale adoption is gone for good, even if the old front comes back.
    await drain(provider, ctx([...history, { role: "assistant", content: "a1" }]))
    expect(JSON.stringify(bodies[1].messages)).not.toContain("compaction")
  })

  it("keeps the history when no summary comes back, and asks again on a later turn", async () => {
    const provider = onDemand()
    const compactions = scriptCreates(provider, [
      { stop_reason: "max_tokens", content: [], usage: {} },
      { stop_reason: "compaction", content: [block], usage: {} },
    ])
    scriptStreams(provider, [textChunks("a1", { input_tokens: 200, output_tokens: 1 })])
    await drain(provider, ctx([{ role: "user", content: "u1" }]))
    await provider.compactionIdle()

    const turns: ModelMessage[] = [{ role: "user", content: "u1" }, { role: "assistant", content: "a1" }, { role: "user", content: "u2" }]
    const bodies = scriptStreams(provider, [textChunks("a2", { input_tokens: 200, output_tokens: 1 })])
    await drain(provider, ctx(turns))
    expect(bodies[0].betas).toBeUndefined()
    await provider.compactionIdle()
    expect(compactions).toHaveLength(2)
  })

  it("leaves out the fields the API rejects on a summarization request", async () => {
    const provider = onDemand()
    const compactions = scriptCreates(provider, [{ stop_reason: "compaction", content: [block], usage: {} }])
    const extensions = {
      stop_sequences: ["END"],
      tool_choice: { type: "any" },
      output_config: { effort: "high", format: { type: "json_schema" }, task_budget: { type: "tokens", total: 30_000, remaining: 10_000 } },
    }
    ;(provider as unknown as { streamMessage: () => AsyncIterable<unknown> }).streamMessage = () =>
      (async function*() { for (const chunk of textChunks("a1", { input_tokens: 500, output_tokens: 1 })) yield chunk })()
    for await (const _ of provider.stream(ctx([{ role: "user", content: "u1" }]), [], extensions)) void _
    await provider.compactionIdle()
    const request = compactions[0]
    expect(request.stop_sequences).toBeUndefined()
    expect(request.tool_choice).toBeUndefined()
    expect(request.output_config).toEqual({ effort: "high", task_budget: { type: "tokens", total: 30_000 } })
  })
})

describe("replay blocks are not annotated in place", () => {
  it("a request's cache markers never land on the stored replay", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-fake", model: "claude-opus-5-5", compaction: true })
    const reply: ModelMessage = { role: "assistant", content: "r" }
    const native = [{ type: "compaction", content: "S." }, { type: "text", text: "r" }]
    provider.seedProviderReplay(reply, { protocol: "anthropic-messages", native_blocks: native })
    const turns: ModelMessage[] = [{ role: "user", content: "u1" }, reply]
    for (let i = 0; i < 3; i++) {
      scriptStreams(provider, [textChunks("x")])
      await drain(provider, ctx(turns))
    }
    expect(provider.peekProviderReplay(reply)?.native_blocks).toEqual(native)
  })

  it("counts a history that carries a compaction block under its beta", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-fake", model: "claude-opus-5-5", compaction: true })
    const reply: ModelMessage = { role: "assistant", content: "r" }
    provider.seedProviderReplay(reply, {
      protocol: "anthropic-messages", native_blocks: [{ type: "compaction", content: "S." }, { type: "text", text: "r" }],
    })
    const counted: Body[] = []
    const client = (provider as unknown as { client: { beta: { messages: { countTokens: unknown } } } }).client
    client.beta.messages.countTokens = async (body: Body) => {
      counted.push(body)
      return { input_tokens: 42 }
    }
    const measurement = await provider.countTokens(ctx([{ role: "user", content: "u1" }, reply, { role: "user", content: "u2" }]), [])
    expect(measurement.inputTokens).toBe(42)
    expect(counted[0].betas).toEqual(["compact-2026-01-12"])
    expect(counted[0].messages[0].content[0].type).toBe("compaction")
  })
})
