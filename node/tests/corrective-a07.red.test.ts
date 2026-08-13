import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import {
  openAIChatDialects,
  type OpenAIChatWireDialect,
} from "../src/providers/openai-chat-dialects.js"
import type { CanonicalAdapterInput } from "../src/providers/content-normalization.js"

function input(
  dialect: OpenAIChatWireDialect,
  extensions: Record<string, unknown> = {},
): CanonicalAdapterInput {
  return {
    context: {
      systemText: "system",
      turns: [{ role: "user", blocks: [{ type: "text", text: "hi" }], contentForm: "text" }],
    },
    tools: [{ name: "lookup", description: "Lookup", parameters: '{"type":"object"}' }],
    resolved: {
      identity: {
        providerId: dialect.providerId,
        modelId: "fixture-model",
        endpointId: dialect.endpointId,
        protocol: "openai-chat",
      },
    },
    extensions,
  } as unknown as CanonicalAdapterInput
}

describe("SPC-013 A-07 OpenAI Chat WireDialect", () => {
  it.each([
    ["openai", "openai.chat"],
    ["deepseek", "deepseek.openai"],
    ["kimi", "kimi.openai"],
    ["qwen", "qwen.dashscope"],
    ["glm", "glm.openai"],
    ["minimax", "minimax.openai"],
  ])("binds the %s request policy as dialect data", (id, endpointId) => {
    expect(openAIChatDialects[id]).toMatchObject({ id, providerId: id, endpointId })
  })

  it("implements request/complete/stream lifecycle through the ProtocolAdapter contract", () => {
    const adapter = new OpenAIChatAdapter()
    const dialect = openAIChatDialects.deepseek
    const canonical = input(dialect, { reasoningEffort: "max", thinking: false })
    const request = adapter.buildRequest(canonical, dialect)
    expect(request.params).toMatchObject({
      model: "fixture-model",
      reasoning_effort: "max",
      extra_body: { thinking: { type: "disabled" } },
      messages: [
        { role: "system", content: "system" },
        { role: "user", content: "hi" },
      ],
    })
    expect(request.params).not.toHaveProperty("prompt_cache_key")

    expect(adapter.decodeComplete({
      choices: [{ message: {
        content: "done",
        reasoning_content: "plan",
        tool_calls: [{
          id: "call_1",
          type: "function",
          function: { name: "lookup", arguments: "{}" },
        }],
      }}],
      usage: { completion_tokens: 4, total_tokens: 10 },
    }, { input: canonical }, dialect)).toEqual({
      message: {
        role: "assistant",
        content: "done",
        tokenCount: 4,
        toolCalls: [{ id: "call_1", name: "lookup", arguments: "{}" }],
      },
      replay: {
        provider: "deepseek",
        protocol: "openai-chat",
        model: "fixture-model",
        reasoning_content: "plan",
        tool_calls: [{
          id: "call_1",
          type: "function",
          function: { name: "lookup", arguments: "{}" },
        }],
      },
    })
  })

  it("adds a compatible fixture vendor through one dialect object and no provider subclass", async () => {
    const fixture: OpenAIChatWireDialect = {
      id: "fixture",
      providerId: "openai",
      endpointId: "openai.chat",
      descriptor: {
        reasoning: { supported: true, preserveAcrossToolTurns: false },
      },
      prepareExtensions: extensions => ({
        ...extensions,
        fixture_mode: "strict",
      }),
      cacheKey: "none",
      inlineThinkingTags: false,
      exposeReasoning: () => false,
      requireReasoningReplay: () => false,
      replay: "generic_stream",
    }
    const provider = new OpenAIChatProvider({
      apiKey: "k",
      model: "fixture-model",
      retry: { maxRetries: 1, baseDelay: 0 },
      baseURL: "https://fixture.invalid/v1",
      runtimePolicy: {},
      dialect: fixture,
    })
    let request: Record<string, unknown> | undefined
    ;(provider as any).client = {
      chat: { completions: { create: async (params: Record<string, unknown>) => {
        request = params
        return { choices: [{ message: { content: "ok", tool_calls: [] } }], usage: { total_tokens: 1 } }
      } } },
    }
    await expect(provider.complete({
      systemText: "system",
      turns: [{ role: "user", content: "hi" }],
    }, [])).resolves.toMatchObject({ content: "ok" })
    expect(provider.descriptor?.()).toMatchObject({ provider: "openai", model: "fixture-model" })
    expect(request).toMatchObject({ fixture_mode: "strict", model: "fixture-model" })
  })

  it("keeps the adapter source free of vendor identity branches and registry lookup", async () => {
    const source = await import("node:fs/promises").then(fs => fs.readFile(
      new URL("../src/providers/openai-chat.ts", import.meta.url),
      "utf8",
    ))
    expect(source).not.toMatch(/providerId\s*===|case ["'](?:deepseek|kimi|qwen|glm|minimax)["']/)
    expect(source).not.toMatch(/modelRegistry|resolveProviderRuntime/)
    expect(source).not.toMatch(/private readonly .*replay/i)
    const registry = await import("node:fs/promises").then(fs => fs.readFile(
      new URL("../src/providers/registry.ts", import.meta.url),
      "utf8",
    ))
    expect(registry).not.toMatch(/new (?:DeepSeekProvider|KimiProvider|QwenProvider|GLMProvider|MiniMaxOpenAIProvider)/)
    expect(registry).toMatch(/Object\.values\(openAIChatDialects\)/)
  })
})
