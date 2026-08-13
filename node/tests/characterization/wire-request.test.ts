/**
 * spc_013-A-00: wire-request characterization — for each of the five generation protocols,
 * capture the EXACT request body `complete()` sends (SDK client stubbed, zero network) and
 * the EXACT decoded `Message` it returns for a fixed vendor response.
 *
 * Card 013-A-02..06 (ProtocolAdapter extraction) must reproduce every byte of these goldens
 * (INV-013-01). Two CN vendors are included (deepseek via Anthropic-compatible wire, qwen via
 * OpenAI-chat wire) so dialect/dialect-injection differences are pinned too.
 */
import { AnthropicProvider } from "../../src/providers/anthropic.js"
import { OpenAIChatProvider } from "../../src/providers/openai.js"
import { OpenAIResponsesProvider } from "../../src/providers/openai-responses.js"
import { GeminiProvider } from "../../src/providers/gemini.js"
import { OllamaProvider } from "../../src/providers/ollama.js"
import { createProvider } from "../../src/providers/catalog.js"
import type { LLMProvider, Message } from "../../src/types.js"
import { CHARACTERIZATION_CONTEXT as CTX, CHARACTERIZATION_TOOLS as TOOLS, USAGE } from "./fixtures.js"
import { expectGolden } from "./golden.js"

interface CompleteCapture {
  request: unknown
  message: unknown
  replay: unknown
}

async function captureComplete(
  provider: LLMProvider,
  stub: (provider: LLMProvider, captured: { req?: unknown }) => void,
): Promise<CompleteCapture> {
  const captured: { req?: unknown } = {}
  stub(provider, captured)
  const message = await provider.complete(CTX, TOOLS)
  return {
    request: captured.req ?? null,
    message,
    replay: provider.peekProviderReplay?.(message) ?? null,
  }
}

/* ── per-protocol stubs ─────────────────────────────────────────────────── */

function stubAnthropic(provider: LLMProvider, captured: { req?: unknown }): void {
  const client = (provider as unknown as { client: { messages: { create: unknown } } }).client
  client.messages.create = async (body: unknown) => {
    captured.req = body
    return {
      id: "msg_char",
      type: "message",
      role: "assistant",
      content: [{ type: "text", text: "The weather is sunny." }],
      stop_reason: "end_turn",
      usage: {
        input_tokens: USAGE.input,
        output_tokens: USAGE.output,
        cache_read_input_tokens: USAGE.cacheRead,
        cache_creation_input_tokens: USAGE.cacheCreation,
      },
    }
  }
}

function stubOpenAiChat(provider: LLMProvider, captured: { req?: unknown }): void {
  const client = (provider as unknown as { client: { chat: { completions: { create: unknown } } } }).client
  client.chat.completions.create = async (body: unknown) => {
    captured.req = body
    return {
      id: "chatcmpl-char",
      choices: [{ message: { role: "assistant", content: "The weather is sunny." }, finish_reason: "stop" }],
      usage: {
        prompt_tokens: USAGE.input,
        completion_tokens: USAGE.output,
        total_tokens: USAGE.input + USAGE.output,
        prompt_tokens_details: { cached_tokens: USAGE.cacheRead },
      },
    }
  }
}

function stubOpenAiResponses(provider: LLMProvider, captured: { req?: unknown }): void {
  const client = (provider as unknown as { client: { responses: { create: unknown } } }).client
  client.responses.create = async (body: unknown) => {
    captured.req = body
    return {
      id: "resp_char",
      output: [{ type: "message", content: [{ type: "output_text", text: "The weather is sunny." }] }],
      usage: {
        input_tokens: USAGE.input,
        output_tokens: USAGE.output,
        total_tokens: USAGE.input + USAGE.output,
        input_tokens_details: { cached_tokens: USAGE.cacheRead },
      },
    }
  }
}

function stubGemini(provider: LLMProvider, captured: { req?: unknown }): void {
  (provider as unknown as { genAI: unknown }).genAI = {
    getGenerativeModel: (modelArgs: unknown) => {
      captured.req = { modelArgs }
      return {
        generateContent: async (req: unknown) => {
          captured.req = { modelArgs: captured.req.modelArgs, body: req }
          return {
            response: {
              candidates: [{ content: { parts: [{ text: "The weather is sunny." }] }, finishReason: "STOP" }],
              usageMetadata: {
                promptTokenCount: USAGE.input,
                candidatesTokenCount: USAGE.output,
                totalTokenCount: USAGE.input + USAGE.output,
                cachedContentTokenCount: USAGE.cacheRead,
              },
            },
          }
        },
      }
    },
  }
}

function stubOllama(captured: { req?: unknown }): void {
  const original = globalThis.fetch
  globalThis.fetch = (async (url: unknown, init?: { body?: unknown }) => {
    captured.req = { url: String(url), body: init?.body ? JSON.parse(String(init.body)) : null }
    return {
      ok: true,
      json: async () => ({
        message: { role: "assistant", content: "The weather is sunny." },
        prompt_eval_count: USAGE.input,
        eval_count: USAGE.output,
      }),
    } as unknown as Response
  }) as typeof fetch
  // fetch is restored by the caller via `finally`.
  void original
}

/* ── tests ──────────────────────────────────────────────────────────────── */

describe("spc_013-A-00 characterization: wire request bodies + complete decode", () => {
  it("anthropic-messages (official)", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-char", model: "claude-opus-4-1" })
    expectGolden("wire-anthropic", await captureComplete(provider, stubAnthropic))
  })

  it("anthropic-messages (CN dialect: deepseek via AnthropicCompatibleProvider)", async () => {
    const provider = createProvider({ model: "deepseek/deepseek-chat", apiKey: "sk-char" })
    expectGolden("wire-anthropic-deepseek", await captureComplete(provider, stubAnthropic))
  })

  it("openai-chat (official)", async () => {
    const provider = new OpenAIChatProvider({ apiKey: "sk-char", model: "gpt-4o" })
    expectGolden("wire-openai-chat", await captureComplete(provider, stubOpenAiChat))
  })

  it("openai-chat (CN dialect: qwen)", async () => {
    const provider = createProvider({ model: "qwen/qwen3.7-plus-preview", apiKey: "sk-char", endpoint: "qwen.dashscope" })
    expectGolden("wire-openai-chat-qwen", await captureComplete(provider, stubOpenAiChat))
  })

  it("openai-responses", async () => {
    const provider = new OpenAIResponsesProvider("sk-char", "gpt-4.1")
    expectGolden("wire-openai-responses", await captureComplete(provider, stubOpenAiResponses))
  })

  it("google-generate-content", async () => {
    const provider = new GeminiProvider("sk-char", "gemini-2.0-flash")
    expectGolden("wire-gemini", await captureComplete(provider, stubGemini))
  })

  it("ollama", async () => {
    const provider = new OllamaProvider("llama3")
    const original = globalThis.fetch
    try {
      const captured: { req?: unknown } = {}
      stubOllama(captured)
      const message: Message = await provider.complete(CTX, TOOLS)
      expectGolden("wire-ollama", { request: captured.req ?? null, message, replay: null })
    } finally {
      globalThis.fetch = original
    }
  })
})
