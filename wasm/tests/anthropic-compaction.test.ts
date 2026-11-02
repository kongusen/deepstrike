/**
 * Server-side compaction beyond the plain threshold edit, over the browser-safe fetch transport:
 * a threshold compaction that pauses so the newest history stays verbatim behind the summary, and
 * on-demand compaction requested in the background and swapped in for the messages it summarized.
 * Mirrors node/tests/anthropic-compaction.test.ts.
 */
import { AnthropicProvider } from "../src/providers/anthropic.js"
import type { ModelMessage, RenderedContext } from "../src/types.js"

type Body = Record<string, any>

function sse(events: unknown[]): string {
  return events.map(event => `data: ${JSON.stringify(event)}\n\n`).join("")
}

function text(textValue: string, input = 10, output = 5): string {
  return sse([
    { type: "message_start", message: { usage: { input_tokens: input, output_tokens: 0 } } },
    { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: textValue } },
    { type: "content_block_stop", index: 0 },
    { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: output } },
  ])
}

const PAUSE = sse([
  { type: "message_start", message: { usage: { input_tokens: 0, output_tokens: 0 } } },
  { type: "content_block_start", index: 0, content_block: { type: "compaction", content: null } },
  { type: "content_block_delta", index: 0, delta: { type: "compaction_delta", content: "Summary so far." } },
  { type: "content_block_stop", index: 0 },
  { type: "message_delta", delta: { stop_reason: "compaction" }, usage: { output_tokens: 0 } },
])

const BLOCK = { type: "compaction", content: "Summary of u1..a1.", signature: "sig-1" }

/** Scripted responses in order; streaming bodies get SSE text, others JSON. */
async function withFetch<T>(responses: Array<string | Body>, run: (sent: Body[]) => Promise<T>): Promise<T> {
  const original = globalThis.fetch
  const sent: Body[] = []
  globalThis.fetch = async (_url, init) => {
    sent.push({ ...JSON.parse(String(init?.body)), __beta: new Headers(init?.headers).get("anthropic-beta") })
    const response = responses[sent.length - 1] ?? ""
    return new Response(typeof response === "string" ? response : JSON.stringify(response), { status: 200 })
  }
  try { return await run(sent) } finally { globalThis.fetch = original }
}

async function drain(provider: AnthropicProvider, context: RenderedContext): Promise<string> {
  let out = ""
  for await (const event of provider.stream(context, [])) if (event.type === "text_delta") out += event.delta
  return out
}

const ctx = (turns: ModelMessage[], stateTurn?: ModelMessage): RenderedContext => ({ systemText: "stable", turns, ...(stateTurn ? { stateTurn } : {}) })

test("a paused threshold compaction continues with the block appended, then replays it first", async () => {
  const provider = new AnthropicProvider("k", "claude-opus-5-5", 8096, {
    compaction: { pauseAfterCompaction: { keepRecentMessages: 0 } },
  })
  const first = [{ role: "user", content: "u1" }] as ModelMessage[]
  await withFetch([PAUSE, text("Continued.")], async sent => {
    expect(await drain(provider, ctx(first))).toBe("Continued.")
    expect(sent[0].context_management.edits).toEqual([{ type: "compact_20260112", pause_after_compaction: true }])
    expect(sent[1].context_management.edits).toEqual([{ type: "compact_20260112" }])
    expect(sent[1].messages.at(-1)).toEqual({ role: "assistant", content: [{ type: "compaction", content: "Summary so far." }] })
  })
  await withFetch([text("ok")], async sent => {
    await drain(provider, ctx([...first, { role: "assistant", content: "Continued." }, { role: "user", content: "u2" }]))
    expect(sent[0].messages[0]).toEqual({
      role: "assistant",
      content: [{ type: "compaction", content: "Summary so far." }, { type: "text", text: "Continued." }],
    })
    expect(sent[0].messages).toHaveLength(2)
  })
})

test("kept history follows the block verbatim, without its thinking", async () => {
  const provider = new AnthropicProvider("k", "claude-opus-5-5", 8096, {
    compaction: { pauseAfterCompaction: { keepRecentMessages: 1 } },
  })
  const call = { id: "t1", name: "look", arguments: "{}" }
  const toolTurn: ModelMessage = { role: "assistant", content: "", toolCalls: [call] }
  provider.seedProviderReplay(toolTurn, {
    protocol: "anthropic-messages",
    native_blocks: [{ type: "thinking", thinking: "", signature: "sig" }, { type: "tool_use", id: "t1", name: "look", input: {} }],
  })
  const turns: ModelMessage[] = [
    { role: "user", content: "u1" },
    { role: "assistant", content: "a1" },
    { role: "user", content: "u2" },
    toolTurn,
    { role: "tool", content: "seen", contentParts: [{ type: "tool_result", callId: "t1", output: "seen", isError: false }] } as ModelMessage,
  ]
  const anchor = {
    role: "assistant",
    content: [{ type: "compaction", content: "Summary so far." }, { type: "tool_use", id: "t1", name: "look", input: {} }],
  }
  await withFetch([PAUSE, text("Done.")], async sent => {
    expect(await drain(provider, ctx(turns))).toBe("Done.")
    expect(sent[1].messages[0]).toEqual(anchor)
    expect(sent[1].messages.map((m: Body) => m.role)).toEqual(["assistant", "user"])
  })
  await withFetch([text("ok")], async sent => {
    await drain(provider, ctx([...turns, { role: "assistant", content: "Done." }, { role: "user", content: "u3" }]))
    expect(sent[0].messages[0]).toEqual(anchor)
    expect(sent[0].messages).toHaveLength(4)
  })
})

test("an ordinary response passes through when the pause is configured", async () => {
  const provider = new AnthropicProvider("k", "claude-opus-5-5", 8096, { compaction: { pauseAfterCompaction: { keepRecentMessages: 2 } } })
  await withFetch([text("plain")], async sent => {
    expect(await drain(provider, ctx([{ role: "user", content: "u1" }]))).toBe("plain")
    expect(sent).toHaveLength(1)
  })
})

test("on-demand compaction summarizes a turn's history in the background, then swaps the block in", async () => {
  const provider = new AnthropicProvider("k", "claude-opus-5-5", 8096, {
    compaction: { mode: "on_demand", compactAtInputTokens: 100, instructions: "Keep the open questions." },
  })
  expect(provider.descriptor().nativeContext).toEqual({ compaction: true })
  const history: ModelMessage[] = [{ role: "user", content: "u1" }, { role: "assistant", content: "a1" }, { role: "user", content: "u2" }]
  const state: ModelMessage = { role: "user", content: "[TASK STATE] t" }
  await withFetch([text("a2", 90, 20), { stop_reason: "compaction", content: [BLOCK], usage: {} }, text("a3")], async sent => {
    await drain(provider, ctx(history, state))
    await provider.compactionIdle()
    const request = sent[1]
    expect(request.compaction).toEqual({ type: "summarize", instructions: "Keep the open questions." })
    expect(request.stream).toBeUndefined()
    expect(request.betas).toBeUndefined()
    expect(request.__beta).toBe("prompt-caching-2024-07-31,compact-2026-09-04")
    expect(request.messages.map((m: Body) => m.role)).toEqual(["user", "assistant", "user"])
    expect(JSON.stringify(request.messages)).not.toContain("TASK STATE")

    await drain(provider, ctx([...history, { role: "assistant", content: "a2" }, { role: "user", content: "u3" }], state))
    expect(sent[2].__beta).toBe("prompt-caching-2024-07-31,compact-2026-09-04")
    expect(sent[2].messages[0]).toEqual({ role: "assistant", content: [BLOCK] })
    expect(sent[2].messages.slice(1).map((m: Body) => m.role)).toEqual(["assistant", "user", "user"])
  })
})

test("on-demand compaction sends the whole history once it no longer matches the summary", async () => {
  const provider = new AnthropicProvider("k", "claude-opus-5-5", 8096, { compaction: { mode: "on_demand", compactAtInputTokens: 100 } })
  await withFetch([text("a1", 200, 1), { stop_reason: "compaction", content: [BLOCK], usage: {} }, text("ok")], async sent => {
    await drain(provider, ctx([{ role: "user", content: "u1" }]))
    await provider.compactionIdle()
    await drain(provider, ctx([{ role: "user", content: "renewed goal" }]))
    expect(sent[2].__beta).toBe("prompt-caching-2024-07-31")
    expect(JSON.stringify(sent[2].messages)).not.toContain("compaction")
  })
})
