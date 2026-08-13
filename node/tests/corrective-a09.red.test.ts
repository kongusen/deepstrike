import { AnthropicMessagesAdapter } from "../src/providers/anthropic-adapter.js"
import { GeminiAdapter } from "../src/providers/gemini-adapter.js"
import { OllamaAdapter } from "../src/providers/ollama-adapter.js"
import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import { OpenAIResponsesAdapter } from "../src/providers/openai-responses-adapter.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import type { CanonicalAdapterInput } from "../src/providers/content-normalization.js"
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

const canonicalInput = {
  context: { systemText: "", turns: [] },
  tools: [],
  resolved: { identity: { providerId: "fixture", modelId: "fixture" } },
  extensions: {},
} as unknown as CanonicalAdapterInput

async function run(provider: LLMProvider, sessionId: string): Promise<StreamEvent[]> {
  const runner = new RuntimeRunner({
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 8_000,
    maxTurns: 4,
  } as never)
  const events: StreamEvent[] = []
  for await (const event of runner.run({ sessionId, goal: "continue if truncated" })) {
    events.push(event)
  }
  return events
}

describe("SPC-013 A-09 canonical stop reasons", () => {
  it.each([
    [new AnthropicMessagesAdapter(), "end_turn", "end_turn"],
    [new AnthropicMessagesAdapter(), "tool_use", "tool_use"],
    [new AnthropicMessagesAdapter(), "max_tokens", "max_tokens"],
    [new AnthropicMessagesAdapter(), "stop_sequence", "stop_sequence"],
    [new OpenAIChatAdapter(), "stop", "end_turn"],
    [new OpenAIChatAdapter(), "length", "max_tokens"],
    [new OpenAIChatAdapter(), "tool_calls", "tool_use"],
    [new OpenAIChatAdapter(), "function_call", "tool_use"],
    [new OpenAIChatAdapter(), "content_filter", "content_filter"],
    [new OpenAIResponsesAdapter(), "max_output_tokens", "max_tokens"],
    [new OpenAIResponsesAdapter(), "content_filter", "content_filter"],
    [new GeminiAdapter(), "STOP", "end_turn"],
    [new GeminiAdapter(), "FINISH_REASON_STOP", "end_turn"],
    [new GeminiAdapter(), "MAX_TOKENS", "max_tokens"],
    [new GeminiAdapter(), "SAFETY", "content_filter"],
    [new OllamaAdapter(), "stop", "end_turn"],
    [new OllamaAdapter(), "length", "max_tokens"],
  ])("maps %s raw %s to %s", (adapter, raw, expected) => {
    expect(adapter.normalizeStopReason(raw)).toBe(expected)
  })

  it.each([
    new AnthropicMessagesAdapter(),
    new OpenAIChatAdapter(),
    new OpenAIResponsesAdapter(),
    new GeminiAdapter(),
    new OllamaAdapter(),
  ])("maps unknown values to other", adapter => {
    expect(adapter.normalizeStopReason("vendor_future_value")).toBe("other")
  })

  it("emits canonical + raw values for Anthropic", () => {
    const adapter = new AnthropicMessagesAdapter()
    const state = adapter.createStreamState({ input: canonicalInput })
    const output = adapter.pushStreamChunk({
      type: "message_delta",
      delta: { stop_reason: "max_tokens" },
      usage: { output_tokens: 7 },
    }, state)
    expect(output.events).toContainEqual(expect.objectContaining({
      type: "usage",
      stopReason: "max_tokens",
      rawStopReason: "max_tokens",
    }))
  })

  it("emits canonical + raw values for OpenAI Chat", () => {
    const adapter = new OpenAIChatAdapter()
    const state = adapter.createStreamState({ input: canonicalInput })
    adapter.pushStreamChunk({ choices: [{ delta: {}, finish_reason: "length" }] }, state)
    adapter.pushStreamChunk({
      choices: [],
      usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
    }, state)
    expect(adapter.finishStream(state).events).toContainEqual(expect.objectContaining({
      type: "usage",
      stopReason: "max_tokens",
      rawStopReason: "length",
    }))
  })

  it.each([
    ["response.incomplete", "max_output_tokens", "max_tokens"],
    ["response.incomplete", "content_filter", "content_filter"],
  ])("emits Responses %s reason %s", (type, raw, expected) => {
    const adapter = new OpenAIResponsesAdapter()
    const state = adapter.createStreamState({ input: canonicalInput })
    const output = adapter.pushStreamChunk({
      type,
      response: {
        id: "resp_1",
        incomplete_details: { reason: raw },
        usage: { input_tokens: 3, output_tokens: 2, total_tokens: 5 },
      },
    }, state)
    expect(output.events).toContainEqual(expect.objectContaining({
      type: "usage",
      stopReason: expected,
      rawStopReason: raw,
    }))
  })

  it("emits Ollama done_reason", () => {
    const adapter = new OllamaAdapter()
    const state = adapter.createStreamState({ input: canonicalInput })
    const terminal = { done: true, done_reason: "length", prompt_eval_count: 3, eval_count: 2 }
    adapter.pushStreamChunk(terminal, state)
    expect(adapter.finishStream(state, terminal).events).toContainEqual(expect.objectContaining({
      type: "usage",
      stopReason: "max_tokens",
      rawStopReason: "length",
    }))
  })

  it("drives kernel continuation from OpenAI raw length after adapter normalization", async () => {
    const provider = new OpenAIChatProvider({ apiKey: "k", model: "fixture", retry: { maxRetries: 1, baseDelay: 0 } })
    let calls = 0
    ;(provider as any).client = {
      chat: { completions: { create: async () => {
        calls += 1
        const finishReason = calls === 1 ? "length" : "stop"
        return {
          async *[Symbol.asyncIterator]() {
            yield { choices: [{ delta: { content: `part${calls} ` }, finish_reason: null }] }
            yield { choices: [{ delta: {}, finish_reason: finishReason }] }
            yield {
              choices: [],
              usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
            }
          },
        }
      } } },
    }

    const events = await run(provider, "a09-openai-length")
    expect(calls).toBe(2)
    expect(events.find(event => event.type === "done")).toMatchObject({ status: "completed" })
  })

  it("never lets rawStopReason override the canonical runner value", async () => {
    class ConflictingRawProvider implements LLMProvider {
      calls = 0
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      }
      async *stream(): AsyncIterable<StreamEvent> {
        this.calls += 1
        yield { type: "text_delta", delta: "done" }
        yield {
          type: "usage",
          totalTokens: 1,
          stopReason: "end_turn",
          rawStopReason: "MAX_TOKENS",
        }
      }
    }
    const provider = new ConflictingRawProvider()
    const events = await run(provider, "a09-raw-is-diagnostic")
    expect(provider.calls).toBe(1)
    expect(events.find(event => event.type === "done")).toMatchObject({ status: "completed" })
  })
})
