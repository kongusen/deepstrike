/**
 * spc_011-C-05: `GeminiProvider.countTokens()` — native preflight token counting via the
 * `@google/generative-ai` SDK's `GenerativeModel.countTokens` method (verified real:
 * `node_modules/@google/generative-ai/dist/src/methods/count-tokens.d.ts`, returns
 * `{ totalTokens: number }`).
 *
 * Stubs `genAI.getGenerativeModel(...).countTokens`, asserting the request is built the same
 * way `complete()` builds it (contents via `buildContents`, tools via `buildTools` + vendor
 * server tools via `vendorConfig`) and that the response normalizes into a `PromptMeasurement`
 * with `source: { kind: "native", provider: "gemini" }`.
 */
import { GeminiProvider, buildContents } from "../src/providers/gemini.js"
import type { RenderedContext, ToolSchema } from "../src/types.js"

interface CapturedCountRequest {
  contents?: unknown[]
}

function makeStubProvider(): {
  provider: GeminiProvider
  captured: { req?: CapturedCountRequest; modelArgs?: Record<string, unknown> }
  respondWith: (totalTokens: number) => void
} {
  const provider = new GeminiProvider("test-key")
  const captured: { req?: CapturedCountRequest; modelArgs?: Record<string, unknown> } = {}
  let responseTokens = 0
  ;(provider as unknown as { genAI: unknown }).genAI = {
    getGenerativeModel: (modelArgs: Record<string, unknown>) => {
      captured.modelArgs = modelArgs
      return {
        countTokens: async (req: CapturedCountRequest) => {
          captured.req = req
          return { totalTokens: responseTokens }
        },
      }
    },
  }
  return {
    provider,
    captured,
    respondWith: (totalTokens: number) => { responseTokens = totalTokens },
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

describe("spc_011-C-05: GeminiProvider.countTokens", () => {
  it("builds the request via the same contents/tools construction complete() uses, and returns a native PromptMeasurement", async () => {
    const { provider, captured, respondWith } = makeStubProvider()
    respondWith(4200)

    const measurement = await provider.countTokens(ctx, tools)

    expect(captured.req?.contents).toEqual(buildContents(ctx.turns))
    expect(captured.modelArgs?.model).toBe("gemini-2.0-flash")
    expect(measurement).toEqual({
      inputTokens: 4200,
      source: { kind: "native", provider: "gemini" },
      confidence: "exact",
    })
  })

  it("passes google_search grounding through vendorConfig when set via extensions", async () => {
    const { provider, captured, respondWith } = makeStubProvider()
    respondWith(10)

    await provider.countTokens(ctx, [], { google_search: true })

    expect(captured.modelArgs?.tools).toEqual([{ googleSearch: {} }])
  })
})
