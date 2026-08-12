import type { GenerateContentResponse } from "@google/generative-ai"

import { GeminiAdapter } from "../src/providers/gemini-adapter.js"
import { normalizeCanonicalAdapterInput } from "../src/providers/content-normalization.js"
import { resolveProviderRuntime } from "../src/providers/catalog.js"
import type { RenderedContext, ToolSchema } from "../src/types.js"

const context: RenderedContext = {
  systemText: "You are precise.",
  turns: [
    { role: "user", content: "Weather in Paris?" },
    {
      role: "assistant",
      content: "",
      toolCalls: [{ id: "call_1", name: "get_weather", arguments: '{"city":"Paris"}' }],
    },
    {
      role: "tool",
      content: "sunny",
      contentParts: [{
        type: "tool_result",
        callId: "call_1",
        output: "sunny",
        isError: false,
      }],
    },
  ],
}

const tools: ToolSchema[] = [{
  name: "get_weather",
  description: "Get weather",
  parameters: '{"type":"object","properties":{"city":{"type":"string"}}}',
}]

function input(extensions: Record<string, unknown> = {}) {
  const resolved = resolveProviderRuntime({
    model: "gemini/gemini-2.0-flash",
    apiKey: "test-key",
  })
  return normalizeCanonicalAdapterInput({ context, tools, resolved, extensions })
}

describe("SPC-013 A-03 Gemini ProtocolAdapter lifecycle", () => {
  it("builds the typed model/request plan from canonical input", () => {
    const adapter = new GeminiAdapter()
    expect(adapter.protocol).toBe("gemini")
    expect(adapter.buildRequest(input({
      google_search: true,
      response_mime_type: "application/json",
      temperature: 0.2,
    }))).toEqual({
      modelParams: {
        model: "gemini-2.0-flash",
        systemInstruction: "You are precise.",
        tools: [
          {
            functionDeclarations: [{
              name: "get_weather",
              description: "Get weather",
              parameters: { type: "object", properties: { city: { type: "string" } } },
            }],
          },
          { googleSearch: {} },
        ],
        generationConfig: { responseMimeType: "application/json" },
        temperature: 0.2,
      },
      request: {
        contents: [
          { role: "user", parts: [{ text: "Weather in Paris?" }] },
          { role: "model", parts: [{ functionCall: { name: "get_weather", args: { city: "Paris" } } }] },
          { role: "user", parts: [{ functionResponse: { name: "get_weather", response: { output: "sunny" } } }] },
        ],
      },
    })
  })

  it("decodes a complete response without transport responsibilities", () => {
    const adapter = new GeminiAdapter()
    const raw = {
      candidates: [{
        index: 0,
        content: {
          role: "model",
          parts: [
            { text: "Checking." },
            { functionCall: { name: "get_weather", args: { city: "Paris" } } },
          ],
        },
        finishReason: "STOP",
      }],
      usageMetadata: {
        promptTokenCount: 80,
        candidatesTokenCount: 25,
        totalTokenCount: 105,
      },
    } as GenerateContentResponse
    expect(adapter.decodeComplete(raw, { input: input() })).toEqual({
      message: {
        role: "assistant",
        content: "Checking.",
        tokenCount: 25,
        toolCalls: [{
          id: "get_weather",
          name: "get_weather",
          arguments: '{"city":"Paris"}',
        }],
      },
    })
  })

  it("keeps chunks incremental and finalizes buffered calls, usage, and finish reason from final response", async () => {
    const adapter = new GeminiAdapter()
    const adapterInput = input()
    const state = adapter.createStreamState({ input: adapterInput })

    expect(adapter.pushStreamChunk({
      candidates: [{
        index: 0,
        content: { role: "model", parts: [{ text: "Checking." }] },
      }],
    }, state)).toEqual({
      events: [{ type: "text_delta", delta: "Checking." }],
    })
    expect(adapter.pushStreamChunk({
      candidates: [{
        index: 0,
        content: {
          role: "model",
          parts: [{ functionCall: { name: "get_weather", args: { city: "Paris" } } }],
        },
      }],
    }, state)).toEqual({ events: [] })

    const final = {
      candidates: [{
        index: 0,
        content: { role: "model", parts: [] },
        finishReason: "MAX_TOKENS",
      }],
      usageMetadata: {
        promptTokenCount: 80,
        candidatesTokenCount: 25,
        totalTokenCount: 105,
        cachedContentTokenCount: 60,
      },
    } as GenerateContentResponse
    expect(await adapter.finishStream(state, final)).toEqual({
      events: [
        {
          type: "tool_call",
          id: "call_1",
          name: "get_weather",
          arguments: { city: "Paris" },
        },
        {
          type: "usage",
          totalTokens: 105,
          inputTokens: 80,
          outputTokens: 25,
          cacheReadInputTokens: 60,
          providerUsage: { inputTokens: 80, outputTokens: 25, cacheReadInputTokens: 60 },
          stopReason: "max_tokens",
          rawStopReason: "MAX_TOKENS",
        },
      ],
    })
  })

  it("rejects a present but malformed usage shape", () => {
    const adapter = new GeminiAdapter()
    expect(() => adapter.normalizeUsage({
      promptTokenCount: "80",
      candidatesTokenCount: 25,
    })).toThrow(/usage.*promptTokenCount/i)
  })

  it("keeps the adapter Registry-independent and binds one resolved runtime into transport", () => {
    const here = path.dirname(fileURLToPath(import.meta.url))
    const source = fs.readFileSync(
      path.join(here, "../src/providers/gemini-adapter.ts"),
      "utf8",
    )
    expect(source).not.toMatch(/modelRegistry|resolveProviderRuntime|getRuntimePolicy/)

    const resolved = resolveProviderRuntime({
      model: "gemini/gemini-2.0-flash",
      apiKey: "test-key",
    })
    expect((resolved.adapter as unknown as { resolvedRuntime: unknown }).resolvedRuntime)
      .toBe(resolved)
  })
})
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
