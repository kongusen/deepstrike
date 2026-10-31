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
  expect(body.context_management).toEqual({
    edits: [{ type: "clear_tool_uses_20250919", trigger: { type: "input_tokens", value: 60_000 }, keep: { type: "tool_uses", value: 4 } }],
  })
})

test("native tiers lower onto the kernel's delegated_tiers, and nothing when none", () => {
  expect(delegatedContextTiers(undefined)).toBeUndefined()
  expect(delegatedContextTiers({ toolResultClearing: true })).toEqual({ tool_result_clearing: true })
})
