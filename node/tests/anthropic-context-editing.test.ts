/**
 * Delegated tool-result clearing: an Anthropic provider configured with context editing declares
 * the tier native, sends `clear_tool_uses_20250919` on every request, and the runner hands the
 * tier to the provider so the kernel stops clearing tool results itself.
 */
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { delegatedContextTiers } from "../src/runtime/runner.js"
import type { RenderedContext } from "../src/types.js"

const context: RenderedContext = {
  systemText: "stable",
  systemStable: "stable",
  turns: [{ role: "user", content: "hi" }],
}

async function captured(provider: AnthropicProvider, extensions?: Record<string, unknown>): Promise<Record<string, unknown>> {
  let body: Record<string, unknown> = {}
  ;(provider as unknown as { streamMessage: (params: Record<string, unknown>) => AsyncIterable<unknown> }).streamMessage = params => {
    body = params
    return (async function*() { /* empty stream */ })()
  }
  for await (const _ of provider.stream(context, [], extensions)) void _
  return body
}

describe("Anthropic context editing delegates tool-result clearing", () => {
  it("sends nothing and declares nothing when it is off", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-fake", model: "claude-opus-5-5" })
    expect(provider.descriptor().nativeContext).toBeUndefined()
    const body = await captured(provider)
    expect(body.context_management).toBeUndefined()
    expect(body.betas).toBeUndefined()
  })

  it("sends clear_tool_uses with the configured settings and declares the tier", async () => {
    const provider = new AnthropicProvider({
      apiKey: "sk-fake",
      model: "claude-opus-5-5",
      contextEditing: { triggerInputTokens: 60_000, keepToolUses: 4, clearAtLeastInputTokens: 5_000, excludeTools: ["memory"] },
    })
    expect(provider.descriptor().nativeContext).toEqual({ toolResultClearing: true })
    const body = await captured(provider)
    expect(body.betas).toEqual(["context-management-2025-06-27"])
    expect(body.context_management).toEqual({
      edits: [{
        type: "clear_tool_uses_20250919",
        trigger: { type: "input_tokens", value: 60_000 },
        keep: { type: "tool_uses", value: 4 },
        clear_at_least: { type: "input_tokens", value: 5_000 },
        exclude_tools: ["memory"],
      }],
    })
  })

  it("keeps the caller's betas and edits beside its own", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-fake", model: "claude-opus-5-5", contextEditing: true })
    const body = await captured(provider, {
      betas: ["other-beta"],
      context_management: { edits: [{ type: "clear_thinking_20251015" }] },
    })
    expect(body.betas).toEqual(["other-beta", "context-management-2025-06-27"])
    expect(body.context_management).toEqual({
      edits: [{ type: "clear_thinking_20251015" }, { type: "clear_tool_uses_20250919" }],
    })
  })

  it("lowers native tiers onto the kernel's delegated_tiers, and nothing when none", () => {
    expect(delegatedContextTiers(undefined)).toBeUndefined()
    expect(delegatedContextTiers({})).toBeUndefined()
    expect(delegatedContextTiers({ toolResultClearing: true })).toEqual({ tool_result_clearing: true })
    expect(delegatedContextTiers({ toolResultClearing: true, compaction: true }))
      .toEqual({ tool_result_clearing: true, compaction: true })
  })
})
