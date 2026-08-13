/**
 * spc_011-C-03: `AnthropicProvider.countTokens()` — native preflight token counting via the
 * Anthropic SDK's `messages.countTokens` endpoint.
 *
 * Stubs the underlying client call (never makes a real API call), asserting both that the
 * request body is built the same way `complete()` builds it (system/messages/tools) and that
 * the response is normalized into a `PromptMeasurement` with `source: { kind: "native" }`.
 */
import { AnthropicProvider } from "../src/providers/anthropic.js"
import type { RenderedContext, ToolSchema } from "../src/types.js"

interface CapturedCountRequest {
  model?: string
  system?: unknown
  tools?: unknown[]
  messages?: unknown[]
}

function makeStubProvider(): {
  provider: AnthropicProvider
  captured: { req?: CapturedCountRequest }
  respondWith: (inputTokens: number) => void
} {
  const provider = new AnthropicProvider({ apiKey: "sk-fake", model: "claude-opus-5" })
  const captured: { req?: CapturedCountRequest } = {}
  let responseTokens = 0
  const client = (provider as unknown as { client: { messages: { countTokens: unknown } } }).client
  client.messages.countTokens = async (body: CapturedCountRequest) => {
    captured.req = body
    return { input_tokens: responseTokens }
  }
  return {
    provider,
    captured,
    respondWith: (inputTokens: number) => { responseTokens = inputTokens },
  }
}

const tools: ToolSchema[] = [
  { name: "search", description: "search the web", parameters: '{"type":"object","properties":{}}' },
]

const ctx: RenderedContext = {
  systemText: "stable\n\nknowledge",
  systemStable: "stable",
  systemKnowledge: "knowledge",
  turns: [
    { role: "user", content: "hi" },
    { role: "assistant", content: "ok" },
  ],
}

describe("spc_011-C-03: AnthropicProvider.countTokens", () => {
  it("builds the request via the same private helpers complete() uses, and returns a native PromptMeasurement", async () => {
    const { provider, captured, respondWith } = makeStubProvider()
    respondWith(4200)

    // Structural equivalence, not a hand-written literal: `buildMessages`/`buildSystem` already
    // have their own dedicated cache-breakpoint coverage (anthropic-cache-strategy.test.ts) —
    // this test only needs to prove `countTokens` routes through the same private helpers
    // `complete()` does, not re-verify their internal cache_control shape.
    const helpers = provider as unknown as {
      buildSystem: (ctxArg: RenderedContext, strategy: string) => unknown
      buildMessages: (ctxArg: RenderedContext, strategy: string) => unknown
    }
    const expectedSystem = helpers.buildSystem(ctx, "default")
    const expectedMessages = helpers.buildMessages(ctx, "default")

    const measurement = await provider.countTokens(ctx, tools)

    expect(captured.req?.model).toBe("claude-opus-5")
    expect(captured.req?.system).toEqual(expectedSystem)
    expect(captured.req?.messages).toEqual(expectedMessages)
    expect(Array.isArray(captured.req?.tools)).toBe(true)
    expect((captured.req?.tools as Array<{ name: string }>)[0].name).toBe("search")

    expect(measurement).toEqual({
      inputTokens: 4200,
      source: { kind: "native", provider: "anthropic" },
      confidence: "exact",
    })
  })

  it("omits the tools field when there are no tools, matching complete()'s convention", async () => {
    const { provider, captured, respondWith } = makeStubProvider()
    respondWith(10)

    await provider.countTokens(ctx, [])

    expect(captured.req?.tools).toBeUndefined()
  })
})
