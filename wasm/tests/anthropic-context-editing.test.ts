import { AnthropicProvider } from "../src/providers/anthropic.js"
import { delegatedContextTiers } from "../src/runtime/runner.js"
import type { RenderedContext } from "../src/types.js"

const context: RenderedContext = { turns: [{ role: "user", content: "question" }] }

async function sent(provider: AnthropicProvider): Promise<{ body: Record<string, unknown>; beta: string | null }> {
  const originalFetch = globalThis.fetch
  let body: Record<string, unknown> = {}
  let beta: string | null = null
  globalThis.fetch = async (_url, init) => {
    body = JSON.parse(String(init?.body))
    beta = new Headers(init?.headers).get("anthropic-beta")
    return new Response("", { status: 200 })
  }
  try { for await (const _ of provider.stream(context, [])) { /* drain */ } } finally { globalThis.fetch = originalFetch }
  return { body, beta }
}

test("context editing is off unless configured", async () => {
  const provider = new AnthropicProvider("test", "claude-opus-5-5")
  expect(provider.descriptor().nativeContext).toBeUndefined()
  const { body, beta } = await sent(provider)
  expect(body.context_management).toBeUndefined()
  expect(beta).toBe("prompt-caching-2024-07-31")
})

test("context editing sends clear_tool_uses and declares the tier native", async () => {
  const provider = new AnthropicProvider("test", "claude-opus-5-5", 8096, {
    contextEditing: { triggerInputTokens: 60_000, keepToolUses: 4 },
  })
  expect(provider.descriptor().nativeContext).toEqual({ toolResultClearing: true })
  const { body, beta } = await sent(provider)
  expect(beta).toBe("prompt-caching-2024-07-31,context-management-2025-06-27")
  // Betas travel in the header only; the Messages API refuses them as a body field.
  expect(body.betas).toBeUndefined()
  expect(body.context_management).toEqual({
    edits: [{ type: "clear_tool_uses_20250919", trigger: { type: "input_tokens", value: 60_000 }, keep: { type: "tool_uses", value: 4 } }],
  })
})

test("compaction sends compact_20260112 beside clear_tool_uses and declares both tiers", async () => {
  const provider = new AnthropicProvider("test", "claude-opus-5-5", 8096, {
    contextEditing: true,
    compaction: { triggerInputTokens: 120_000 },
  })
  expect(provider.descriptor().nativeContext).toEqual({ toolResultClearing: true, compaction: true })
  const { body, beta } = await sent(provider)
  expect(beta).toBe("prompt-caching-2024-07-31,context-management-2025-06-27,compact-2026-01-12")
  expect(body.context_management).toEqual({
    edits: [
      { type: "clear_tool_uses_20250919" },
      { type: "compact_20260112", trigger: { type: "input_tokens", value: 120_000 } },
    ],
  })
})

test("a streamed compaction block is replayed on the next request", async () => {
  const provider = new AnthropicProvider("test", "claude-opus-5-5", 8096, { compaction: true })
  const events = [
    { type: "message_start", message: { usage: { input_tokens: 10, output_tokens: 0 } } },
    { type: "content_block_start", index: 0, content_block: { type: "compaction", content: null } },
    { type: "content_block_delta", index: 0, delta: { type: "compaction_delta", content: "Summary: the user wants a scraper." } },
    { type: "content_block_stop", index: 0 },
    { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: "Here it is." } },
    { type: "content_block_stop", index: 1 },
  ]
  const sse = events.map(event => `data: ${JSON.stringify(event)}\n\n`).join("")
  const bodies: Array<Record<string, unknown>> = []
  const originalFetch = globalThis.fetch
  globalThis.fetch = async (_url, init) => {
    bodies.push(JSON.parse(String(init?.body)))
    return new Response(bodies.length === 1 ? sse : "", { status: 200 })
  }
  try {
    for await (const _ of provider.stream(context, [])) { /* drain */ }
    const next: RenderedContext = {
      turns: [...context.turns, { role: "assistant", content: "Here it is." }, { role: "user", content: "add retries" }],
    }
    for await (const _ of provider.stream(next, [])) { /* drain */ }
  } finally {
    globalThis.fetch = originalFetch
  }
  const messages = bodies[1].messages as Array<{ role: string; content: Array<Record<string, unknown>> }>
  expect(messages[1].role).toBe("assistant")
  expect(messages[1].content.map(block => block.type)).toEqual(["compaction", "text"])
  expect(messages[1].content[0].content).toBe("Summary: the user wants a scraper.")
})

test("native tiers lower onto the kernel's delegated_tiers, and nothing when none", () => {
  expect(delegatedContextTiers(undefined)).toBeUndefined()
  expect(delegatedContextTiers({ toolResultClearing: true })).toEqual({ tool_result_clearing: true })
})
