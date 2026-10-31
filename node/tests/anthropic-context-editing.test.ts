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

describe("Anthropic context editing and compaction delegate their tiers", () => {
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

  it("sends compact_20260112 beside clear_tool_uses and declares both tiers", async () => {
    const provider = new AnthropicProvider({
      apiKey: "sk-fake",
      model: "claude-opus-5-5",
      contextEditing: true,
      compaction: { triggerInputTokens: 120_000, instructions: "Keep the open questions." },
    })
    expect(provider.descriptor().nativeContext).toEqual({ toolResultClearing: true, compaction: true })
    const body = await captured(provider)
    expect(body.betas).toEqual(["context-management-2025-06-27", "compact-2026-01-12"])
    expect(body.context_management).toEqual({
      edits: [
        { type: "clear_tool_uses_20250919" },
        { type: "compact_20260112", trigger: { type: "input_tokens", value: 120_000 }, instructions: "Keep the open questions." },
      ],
    })
  })

  it("replays a streamed compaction block on the next request, ahead of the turn's text", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-fake", model: "claude-opus-5-5", compaction: true })
    const chunks = [
      { type: "message_start", message: { id: "msg_1", usage: { input_tokens: 10, output_tokens: 0 } } },
      { type: "content_block_start", index: 0, content_block: { type: "compaction", content: null } },
      { type: "content_block_delta", index: 0, delta: { type: "compaction_delta", content: "Summary: the user wants a scraper." } },
      { type: "content_block_stop", index: 0 },
      { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: "Here it is." } },
      { type: "content_block_stop", index: 1 },
      { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 5 } },
    ]
    const bodies: Record<string, unknown>[] = []
    ;(provider as unknown as { streamMessage: (params: Record<string, unknown>) => AsyncIterable<unknown> }).streamMessage = params => {
      bodies.push(params)
      return (async function*() { for (const chunk of bodies.length === 1 ? chunks : []) yield chunk })()
    }
    let text = ""
    for await (const event of provider.stream(context, [])) if (event.type === "text_delta") text += event.delta
    expect(text).toBe("Here it is.")

    const next: RenderedContext = {
      ...context,
      turns: [...context.turns, { role: "assistant", content: "Here it is." }, { role: "user", content: "add retries" }],
    }
    for await (const _ of provider.stream(next, [])) void _
    const messages = bodies[1].messages as Array<{ role: string; content: unknown }>
    expect(messages[1]).toEqual({
      role: "assistant",
      content: [
        { type: "compaction", content: "Summary: the user wants a scraper." },
        { type: "text", text: "Here it is." },
      ],
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
