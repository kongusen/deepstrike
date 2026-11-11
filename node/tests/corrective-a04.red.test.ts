import { OllamaAdapter } from "../src/providers/ollama-adapter.js"
import type { CanonicalAdapterInput } from "../src/providers/content-normalization.js"
import { resolveProviderRuntime } from "../src/providers/catalog.js"

const input = {
  context: {
    systemText: "system",
    turns: [{
      role: "user",
      blocks: [{ type: "text", text: "hi" }],
    }],
  },
  tools: [{
    name: "lookup",
    description: "Lookup",
    parameters: '{"type":"object"}',
  }],
  resolved: {
    identity: {
      providerId: "ollama",
      modelId: "llama3",
      endpointId: "ollama.local",
      protocol: "ollama-chat",
    },
  },
  extensions: { temperature: 0.2, model: "wrong", stream: false },
} as unknown as CanonicalAdapterInput

describe("SPC-013 A-04 Ollama ProtocolAdapter lifecycle", () => {
  it("builds the request body from canonical input", () => {
    expect(new OllamaAdapter().buildRequest(input)).toEqual({
      temperature: 0.2,
      model: "llama3",
      messages: [
        { role: "system", content: "system" },
        { role: "user", content: "hi" },
      ],
      tools: [{
        type: "function",
        function: {
          name: "lookup",
          description: "Lookup",
          parameters: { type: "object" },
        },
      }],
    })
  })

  it("buffers calls and finalizes usage/stop reason at EOF", () => {
    const adapter = new OllamaAdapter()
    const state = adapter.createStreamState({ input })
    expect(adapter.pushStreamChunk({
      message: {
        content: "working",
        tool_calls: [{ function: { name: "lookup", arguments: { q: "x" } } }],
      },
    }, state)).toEqual({
      events: [{ type: "text_delta", delta: "working" }],
    })
    expect(adapter.finishStream(state, {
      done: true,
      done_reason: "length",
      prompt_eval_count: 12,
      eval_count: 3,
    })).toEqual({
      events: [
        { type: "tool_call", id: expect.stringMatching(/^call_[a-z0-9]+$/), name: "lookup", arguments: { q: "x" } },
        {
          type: "usage",
          totalTokens: 15,
          inputTokens: 12,
          outputTokens: 3,
          providerUsage: { inputTokens: 12, outputTokens: 3 },
          stopReason: "max_tokens",
          rawStopReason: "length",
        },
      ],
    })
    expect(adapter.normalizeStopReason("length")).toBe("max_tokens")
  })

  it("gives every tool call a run-unique id (the native API gives none)", () => {
    const adapter = new OllamaAdapter()
    const turn = () => {
      const state = adapter.createStreamState({ input })
      adapter.pushStreamChunk({ message: { content: "", tool_calls: [{ function: { name: "lookup", arguments: { q: "x" } } }] } }, state)
      return adapter.finishStream(state, { done: true }).events.find(event => event.type === "tool_call") as { id: string }
    }
    // Two turns each calling one tool used to get `call_1` both times; the kernel ends a run whose
    // call id repeats on a syscall (`invalid_authority`).
    expect(turn().id).not.toBe(turn().id)
    const given = adapter.createStreamState({ input })
    adapter.pushStreamChunk({ message: { content: "", tool_calls: [{ id: "srv-1", function: { name: "lookup", arguments: {} } }] } }, given)
    expect((adapter.finishStream(given, { done: true }).events[0] as { id: string }).id).toBe("srv-1")
    const complete = adapter.decodeComplete({ message: { content: "", tool_calls: [{ function: { name: "lookup", arguments: { q: "y" } } }] } }, { input } as never)
    expect(complete.message.toolCalls).toEqual([{ id: expect.stringMatching(/^call_/), name: "lookup", arguments: "{\"q\":\"y\"}" }])
  })

  it("parses a final NDJSON record even when EOF has no trailing newline", () => {
    const adapter = new OllamaAdapter()
    const decoder = adapter.createNdjsonDecoder()
    expect(decoder.push('{"message":{"content":"tail"}')).toEqual([])
    expect(decoder.finish('}')).toEqual([{ message: { content: "tail" } }])
  })

  it("keeps malformed complete lines on the established skip policy", () => {
    const decoder = new OllamaAdapter().createNdjsonDecoder()
    expect(decoder.push("not-json\n")).toEqual([])
    expect(decoder.finish()).toEqual([])
  })

  it("binds the one resolved runtime into Ollama transport", () => {
    const resolved = resolveProviderRuntime({
      model: "ollama/llama3",
      apiKey: "",
    })
    expect((resolved.adapter as unknown as { resolvedRuntime: unknown }).resolvedRuntime)
      .toBe(resolved)
  })
})
