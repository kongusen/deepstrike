/**
 * spc_013-A-00: stream characterization — for each generation protocol, feed a fixed vendor
 * chunk sequence (text delta + tool call + usage) through the provider's `stream()` and lock
 * the emitted event sequence AND the request params the stream call was built with.
 *
 * A-09 intentionally re-blesses only the stop-reason fields: `stopReason` is canonical across
 * protocols and `rawStopReason` preserves the provider spelling for Node-side diagnostics.
 * Request bodies, delta ordering, tool calls and usage counts remain characterized unchanged.
 */
import { AnthropicProvider } from "../../src/providers/anthropic.js"
import { OpenAIChatProvider } from "../../src/providers/openai.js"
import { OpenAIResponsesProvider } from "../../src/providers/openai-responses.js"
import { GeminiProvider } from "../../src/providers/gemini.js"
import { OllamaProvider } from "../../src/providers/ollama.js"
import type { LLMProvider, StreamEvent } from "../../src/types.js"
import { CHARACTERIZATION_CONTEXT as CTX, CHARACTERIZATION_TOOLS as TOOLS, USAGE } from "./fixtures.js"
import { expectGolden } from "./golden.js"

async function collectStream(provider: LLMProvider, state?: unknown): Promise<StreamEvent[]> {
  const events: StreamEvent[] = []
  const withState = provider as LLMProvider & { createRunState?: () => unknown }
  const runState = state ?? withState.createRunState?.()
  for await (const evt of provider.stream(CTX, TOOLS, undefined, runState as never)) events.push(evt)
  return events
}

/* ── anthropic-messages ─────────────────────────────────────────────────── */

function stubAnthropicStream(provider: LLMProvider, captured: { req?: unknown }, chunks: unknown[]): void {
  const client = (provider as unknown as { client: { messages: { stream: unknown } } }).client
  client.messages.stream = (params: unknown) => {
    captured.req = params
    return {
      async *[Symbol.asyncIterator]() {
        for (const c of chunks) yield c
      },
    }
  }
}

const ANTHROPIC_TOOL_STREAM = [
  { type: "message_start", message: { usage: { input_tokens: USAGE.input, output_tokens: 1, cache_read_input_tokens: USAGE.cacheRead, cache_creation_input_tokens: USAGE.cacheCreation } } },
  { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
  { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "Checking." } },
  { type: "content_block_start", index: 1, content_block: { type: "tool_use", id: "toolu_1", name: "get_weather" } },
  { type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: '{"city":"Paris"}' } },
  { type: "content_block_stop", index: 1 },
  { type: "message_delta", delta: { stop_reason: "tool_use" }, usage: { output_tokens: USAGE.output } },
]

const ANTHROPIC_CAP_STREAM = [
  { type: "message_start", message: { usage: { input_tokens: USAGE.input, output_tokens: 1, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 } } },
  { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
  { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "truncated mid-sen" } },
  { type: "message_delta", delta: { stop_reason: "max_tokens" }, usage: { output_tokens: USAGE.output } },
]

/* ── openai-chat ────────────────────────────────────────────────────────── */

function stubOpenAiChatStream(provider: LLMProvider, captured: { req?: unknown }, chunks: unknown[]): void {
  const client = (provider as unknown as { client: { chat: { completions: { create: unknown } } } }).client
  client.chat.completions.create = async (params: unknown) => {
    captured.req = params
    return {
      async *[Symbol.asyncIterator]() {
        for (const c of chunks) yield c
      },
    }
  }
}

const OPENAI_CHAT_TOOL_STREAM = [
  { choices: [{ delta: { content: "Checking." }, finish_reason: null }] },
  { choices: [{ delta: { tool_calls: [{ index: 0, id: "call_1", function: { name: "get_weather", arguments: '{"city"' } }] }, finish_reason: null }] },
  { choices: [{ delta: { tool_calls: [{ index: 0, function: { arguments: ':"Paris"}' } }] }, finish_reason: null }] },
  { choices: [{ delta: {}, finish_reason: "tool_calls" }] },
  { choices: [], usage: { prompt_tokens: USAGE.input, completion_tokens: USAGE.output, total_tokens: USAGE.input + USAGE.output, prompt_tokens_details: { cached_tokens: USAGE.cacheRead } } },
]

const OPENAI_CHAT_CAP_STREAM = [
  { choices: [{ delta: { content: "truncated mid-sen" }, finish_reason: "length" }] },
  { choices: [], usage: { prompt_tokens: USAGE.input, completion_tokens: USAGE.output, total_tokens: USAGE.input + USAGE.output } },
]

/* ── openai-responses ───────────────────────────────────────────────────── */

function stubResponsesStream(provider: LLMProvider, captured: { req?: unknown }, chunks: unknown[]): void {
  const client = (provider as unknown as { client: { responses: { create: unknown } } }).client
  client.responses.create = async (params: unknown) => {
    captured.req = params
    return {
      async *[Symbol.asyncIterator]() {
        for (const c of chunks) yield c
      },
    }
  }
}

const RESPONSES_TOOL_STREAM = [
  { type: "response.output_text.delta", delta: "Checking." },
  { type: "response.output_item.added", output_index: 0, item: { type: "function_call", call_id: "call_1", name: "get_weather", arguments: "" } },
  { type: "response.function_call_arguments.delta", output_index: 0, delta: '{"city"' },
  { type: "response.function_call_arguments.done", output_index: 0, arguments: '{"city":"Paris"}' },
  { type: "response.output_item.done", output_index: 0, item: { type: "function_call", call_id: "call_1", name: "get_weather", arguments: '{"city":"Paris"}' } },
  {
    type: "response.completed",
    response: {
      id: "resp_char",
      usage: { input_tokens: USAGE.input, output_tokens: USAGE.output, total_tokens: USAGE.input + USAGE.output, input_tokens_details: { cached_tokens: USAGE.cacheRead } },
    },
  },
]

/* ── google-generate-content ────────────────────────────────────────────── */

function stubGeminiStream(provider: LLMProvider, captured: { req?: unknown }, chunks: unknown[]): void {
  (provider as unknown as { genAI: unknown }).genAI = {
    getGenerativeModel: (modelArgs: unknown) => ({
      generateContentStream: async (req: unknown) => {
        captured.req = { modelArgs, body: req }
        return {
          stream: {
            async *[Symbol.asyncIterator]() {
              for (const c of chunks) yield c
            },
          },
          response: Promise.resolve({
            usageMetadata: {
              promptTokenCount: USAGE.input,
              candidatesTokenCount: USAGE.output,
              totalTokenCount: USAGE.input + USAGE.output,
              cachedContentTokenCount: USAGE.cacheRead,
            },
          }),
        }
      },
    }),
  }
}

const GEMINI_TOOL_STREAM = [
  { candidates: [{ content: { parts: [{ text: "Checking." }] } }] },
  { candidates: [{ content: { parts: [{ functionCall: { name: "get_weather", args: { city: "Paris" } } }] } }] },
]

/* ── ollama ─────────────────────────────────────────────────────────────── */

function stubOllamaStream(captured: { req?: unknown }, lines: string[]): void {
  globalThis.fetch = (async (url: unknown, init?: { body?: unknown }) => {
    captured.req = { url: String(url), body: init?.body ? JSON.parse(String(init.body)) : null }
    const payload = new TextEncoder().encode(lines.join("\n") + "\n")
    return {
      ok: true,
      body: new ReadableStream({
        start(controller) {
          controller.enqueue(payload)
          controller.close()
        },
      }),
    } as unknown as Response
  }) as typeof fetch
}

const OLLAMA_TOOL_STREAM = [
  '{"message":{"role":"assistant","content":"Checking."},"done":false}',
  '{"message":{"role":"assistant","tool_calls":[{"function":{"name":"get_weather","arguments":{"city":"Paris"}}}]},"done":false}',
  `{"done":true,"done_reason":"stop","prompt_eval_count":${USAGE.input},"eval_count":${USAGE.output}}`,
]

/* ── tests ──────────────────────────────────────────────────────────────── */

describe("spc_013-A-00 characterization: stream chunk → event sequences", () => {
  it("anthropic-messages: tool-call stream", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-char", model: "claude-opus-4-1" })
    const captured: { req?: unknown } = {}
    stubAnthropicStream(provider, captured, ANTHROPIC_TOOL_STREAM)
    expectGolden("stream-anthropic-tool", { request: captured.req ?? null, events: await collectStream(provider) })
  })

  it("anthropic-messages: canonical stopReason with raw diagnostics", async () => {
    const provider = new AnthropicProvider({ apiKey: "sk-char", model: "claude-opus-4-1" })
    const captured: { req?: unknown } = {}
    stubAnthropicStream(provider, captured, ANTHROPIC_CAP_STREAM)
    expectGolden("stream-anthropic-stopreason-before", { events: await collectStream(provider) })
  })

  it("openai-chat: tool-call stream", async () => {
    const provider = new OpenAIChatProvider({ apiKey: "sk-char", model: "gpt-4o" })
    const captured: { req?: unknown } = {}
    stubOpenAiChatStream(provider, captured, OPENAI_CHAT_TOOL_STREAM)
    expectGolden("stream-openai-chat-tool", { request: captured.req ?? null, events: await collectStream(provider) })
  })

  it("openai-chat: canonical stopReason with raw diagnostics", async () => {
    const provider = new OpenAIChatProvider({ apiKey: "sk-char", model: "gpt-4o" })
    const captured: { req?: unknown } = {}
    stubOpenAiChatStream(provider, captured, OPENAI_CHAT_CAP_STREAM)
    expectGolden("stream-openai-chat-stopreason-before", { events: await collectStream(provider) })
  })

  it("openai-responses: tool-call stream", async () => {
    const provider = new OpenAIResponsesProvider("sk-char", "gpt-4.1")
    const captured: { req?: unknown } = {}
    stubResponsesStream(provider, captured, RESPONSES_TOOL_STREAM)
    expectGolden("stream-openai-responses-tool", { request: captured.req ?? null, events: await collectStream(provider) })
  })

  it("google-generate-content: tool-call stream", async () => {
    const provider = new GeminiProvider("sk-char", "gemini-2.0-flash")
    const captured: { req?: unknown } = {}
    stubGeminiStream(provider, captured, GEMINI_TOOL_STREAM)
    expectGolden("stream-gemini-tool", { request: captured.req ?? null, events: await collectStream(provider) })
  })

  it("ollama: tool-call stream with canonical stopReason", async () => {
    const provider = new OllamaProvider("llama3")
    const original = globalThis.fetch
    try {
      const captured: { req?: unknown } = {}
      stubOllamaStream(captured, OLLAMA_TOOL_STREAM)
      expectGolden("stream-ollama-tool", { request: captured.req ?? null, events: await collectStream(provider) })
    } finally {
      globalThis.fetch = original
    }
  })
})
