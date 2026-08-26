import { readFileSync } from "node:fs"
import { join } from "node:path"
import {
  ANTHROPIC_TEXTUAL_TOOL_CALL_START_MARKER,
  AnthropicMessagesAdapter,
} from "../src/providers/anthropic-adapter.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import type { CanonicalAdapterInput } from "../src/providers/content-normalization.js"
import { endpointProfiles, type EndpointProfileId } from "../src/providers/endpoints.js"
import { ProviderError, classifyProviderError } from "../src/providers/provider-error.js"
import { ProtocolResponseError } from "../src/providers/protocol-adapter.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

const fixture = JSON.parse(readFileSync(join(
  process.cwd(), "../tests/fixtures/provider-textual-tool-call/canonical.json",
), "utf8")) as {
  startMarker: string
  complete: string
  ordinaryText: string
  providerCode: string
  message: string
}
const START_MARKER = fixture.startMarker
const DSML = fixture.complete

function input(endpointId: EndpointProfileId, withTools = true): CanonicalAdapterInput {
  const endpoint = endpointProfiles[endpointId]
  return {
    context: { systemText: "", turns: [] },
    tools: withTools
      ? [{ name: "lookup", description: "Lookup", parameters: '{"type":"object"}' }]
      : [],
    resolved: {
      identity: {
        providerId: endpoint.providerId,
        modelId: endpoint.providerId === "anthropic" ? "claude-sonnet-4-6" : "fixture-model",
        endpointId,
        protocol: "anthropic-messages",
      },
      endpoint,
    },
    extensions: {},
  } as unknown as CanonicalAdapterInput
}

function withPolicy(
  canonical: CanonicalAdapterInput,
  textualToolCallPolicy: "off" | "reject",
): CanonicalAdapterInput {
  return { ...canonical, extensions: { textualToolCallPolicy } }
}

function textualError(fn: () => unknown): ProtocolResponseError {
  try {
    fn()
  } catch (error) {
    expect(error).toBeInstanceOf(ProtocolResponseError)
    return error as ProtocolResponseError
  }
  throw new Error("expected textual tool call rejection")
}

describe("SPC-022 Anthropic textual tool call rejection", () => {
  it("matches the shared Node/Python malformed-output fixture", () => {
    expect(ANTHROPIC_TEXTUAL_TOOL_CALL_START_MARKER).toBe(fixture.startMarker)
  })

  it("rejects exact DSML on compatible endpoints without exposing its body", () => {
    const adapter = new AnthropicMessagesAdapter()
    const error = textualError(() => adapter.decodeComplete({
      content: [{ type: "text", text: `preface ${DSML}` }],
    }, { input: input("deepseek.anthropic") }))

    expect(error).toMatchObject({ providerCode: fixture.providerCode, retryable: true })
    expect(error.message).toBe(fixture.message)
    expect(error.message).not.toContain("secret")
  })

  it("keeps official Anthropic, no-tools, and ordinary XML-shaped text unchanged", () => {
    const adapter = new AnthropicMessagesAdapter()
    expect(adapter.decodeComplete({ content: [{ type: "text", text: DSML }] }, {
      input: input("anthropic.messages"),
    }).message.content).toBe(DSML)
    expect(adapter.decodeComplete({ content: [{ type: "text", text: DSML }] }, {
      input: input("deepseek.anthropic", false),
    }).message.content).toBe(DSML)
    expect(adapter.decodeComplete({ content: [{ type: "text", text: fixture.ordinaryText }] }, {
      input: input("deepseek.anthropic"),
    }).message.content).toBe(fixture.ordinaryText)
  })

  it("rejects DSML even when a native tool call is also present", () => {
    const adapter = new AnthropicMessagesAdapter()
    textualError(() => adapter.decodeComplete({ content: [
      { type: "tool_use", id: "call_1", name: "lookup", input: {} },
      { type: "text", text: DSML },
    ] }, { input: input("minimax.anthropic") }))
  })

  it("allows explicit policy to override endpoint defaults", () => {
    const adapter = new AnthropicMessagesAdapter()
    expect(adapter.decodeComplete({ content: [{ type: "text", text: DSML }] }, {
      input: withPolicy(input("deepseek.anthropic"), "off"),
    }).message.content).toBe(DSML)
    textualError(() => adapter.decodeComplete({ content: [{ type: "text", text: DSML }] }, {
      input: withPolicy(input("anthropic.messages"), "reject"),
    }))
  })

  it("treats a custom Anthropic base URL as reject and keeps policy off the wire", async () => {
    const provider = new AnthropicProvider({
      apiKey: "k",
      baseURL: "https://gateway.example.test/anthropic",
      retry: { maxRetries: 1, baseDelay: 0 },
    })
    let request: Record<string, unknown> | undefined
    ;(provider as unknown as { client: { messages: { create: (params: Record<string, unknown>) => Promise<unknown> } } })
      .client.messages.create = async params => {
        request = params
        return { content: [{ type: "text", text: DSML }] }
      }

    await expect(provider.complete(
      { systemText: "", turns: [{ role: "user", content: "hi" }] },
      [{ name: "lookup", description: "Lookup", parameters: '{"type":"object"}' }],
    )).rejects.toMatchObject({
      name: "ProviderError",
      kind: "protocol",
      providerCode: "textual_tool_call",
      retryable: true,
    } satisfies Partial<ProviderError>)
    expect(request).not.toHaveProperty("textualToolCallPolicy")
  })

  it("treats the official Anthropic base URL with a trailing slash as policy off", async () => {
    const provider = new AnthropicProvider({
      apiKey: "k",
      baseURL: "https://api.anthropic.com/",
      retry: { maxRetries: 1, baseDelay: 0 },
    })
    ;(provider as unknown as { client: { messages: { create: () => Promise<unknown> } } })
      .client.messages.create = async () => ({ content: [{ type: "text", text: DSML }] })

    await expect(provider.complete(
      { systemText: "", turns: [{ role: "user", content: "hi" }] },
      [{ name: "lookup", description: "Lookup", parameters: '{"type":"object"}' }],
    )).resolves.toMatchObject({ content: DSML })
  })

  it("preserves retryable protocol metadata through provider classification", () => {
    const raw = new ProtocolResponseError("anthropic-messages", "safe", {
      providerCode: "textual_tool_call",
      retryable: true,
    })
    expect(classifyProviderError("deepseek", raw)).toMatchObject({
      provider: "deepseek",
      kind: "protocol",
      providerCode: "textual_tool_call",
      retryable: true,
    })
  })

  it("marks Anthropic complete usage cache telemetry as measured or unavailable", () => {
    const adapter = new AnthropicMessagesAdapter()
    expect(adapter.normalizeUsage({
      input_tokens: 10,
      output_tokens: 2,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
    })).toMatchObject({
      cacheTelemetryStatus: "measured",
      cacheTelemetrySource: "anthropic_usage",
    })
    expect(adapter.normalizeUsage({ input_tokens: 10, output_tokens: 2 })).toMatchObject({
      cacheTelemetryStatus: "unavailable",
    })
  })

  it("does not mark Anthropic stream cache telemetry measured when cache fields are absent", () => {
    const adapter = new AnthropicMessagesAdapter()
    const unavailableState = adapter.createStreamState({ input: input("anthropic.messages") })
    const unavailable = adapter.pushStreamChunk({
      type: "message_start",
      message: { usage: { input_tokens: 10, output_tokens: 0 } },
    }, unavailableState).events.at(-1)
    expect(unavailable).toMatchObject({ cacheTelemetryStatus: "unavailable" })
    expect(unavailable).not.toHaveProperty("cacheTelemetrySource")

    const measuredState = adapter.createStreamState({ input: input("anthropic.messages") })
    const measured = adapter.pushStreamChunk({
      type: "message_start",
      message: {
        usage: {
          input_tokens: 10,
          output_tokens: 0,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
        },
      },
    }, measuredState).events.at(-1)
    expect(measured).toMatchObject({
      cacheTelemetryStatus: "measured",
      cacheTelemetrySource: "anthropic_usage",
    })
  })

  it.each(Array.from({ length: START_MARKER.length + 1 }, (_, split) => split))(
    "buffers every marker split position %i without leaking candidate text",
    split => {
      const adapter = new AnthropicMessagesAdapter()
      const state = adapter.createStreamState({ input: input("deepseek.anthropic") })
      const visible: string[] = []
      for (const text of [
        `visible:${START_MARKER.slice(0, split)}`,
        `${START_MARKER.slice(split)}secret-body`,
      ]) {
        for (const event of adapter.pushStreamChunk({
          type: "content_block_delta",
          index: 0,
          delta: { type: "text_delta", text },
        }, state).events) {
          if (event.type === "text_delta") visible.push(event.delta)
        }
      }

      const error = textualError(() => adapter.finishStream(state))
      expect(visible.join("")).toBe("visible:")
      expect(error.message).not.toContain("secret-body")
    },
  )

  it("flushes a harmless buffered marker prefix at EOF", () => {
    const adapter = new AnthropicMessagesAdapter()
    const state = adapter.createStreamState({ input: input("deepseek.anthropic") })
    const pushed = adapter.pushStreamChunk({
      type: "content_block_delta",
      index: 0,
      delta: { type: "text_delta", text: `plain ${START_MARKER.slice(0, -1)}` },
    }, state)
    const finished = adapter.finishStream(state)
    const visible = [...pushed.events, ...finished.events]
      .filter(event => event.type === "text_delta")
      .map(event => event.delta)
      .join("")
    expect(visible).toBe(`plain ${START_MARKER.slice(0, -1)}`)
  })

  it("detects a marker split between content block start and delta", () => {
    const adapter = new AnthropicMessagesAdapter()
    const state = adapter.createStreamState({ input: input("deepseek.anthropic") })
    const split = 8
    const started = adapter.pushStreamChunk({
      type: "content_block_start",
      index: 0,
      content_block: { type: "text", text: `visible:${START_MARKER.slice(0, split)}` },
    }, state)
    const continued = adapter.pushStreamChunk({
      type: "content_block_delta",
      index: 0,
      delta: { type: "text_delta", text: `${START_MARKER.slice(split)}secret-body` },
    }, state)
    const visible = [...started.events, ...continued.events]
      .filter(event => event.type === "text_delta")
      .map(event => event.delta)
      .join("")
    expect(visible).toBe("visible:")
    textualError(() => adapter.finishStream(state))
  })

  it("fails with the same safe error when candidate capture exceeds its bound", () => {
    const adapter = new AnthropicMessagesAdapter()
    const state = adapter.createStreamState({ input: input("deepseek.anthropic") })
    adapter.pushStreamChunk({
      type: "content_block_delta", index: 0,
      delta: { type: "text_delta", text: START_MARKER },
    }, state)
    const error = textualError(() => adapter.pushStreamChunk({
      type: "content_block_delta", index: 0,
      delta: { type: "text_delta", text: "😀".repeat(20_000) },
    }, state))
    expect(error.message).toBe(fixture.message)
  })

  it("reaches Runner recovery without persisting a false llm completion or provider body", async () => {
    const cause = new Error(`untrusted:${DSML}`)
    class TextualToolCallProvider implements LLMProvider {
      async complete(): Promise<Message> {
        throw new Error("unused")
      }
      // eslint-disable-next-line require-yield
      async *stream(): AsyncIterable<StreamEvent> {
        throw new ProviderError({
          provider: "deepseek",
          kind: "protocol",
          retryable: true,
          providerCode: "textual_tool_call",
          message: "Provider emitted a tool call as text instead of a native tool block",
          cause,
        })
      }
    }

    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: new TextualToolCallProvider(),
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8_000,
      maxTurns: 3,
    } as never)
    const events: StreamEvent[] = []
    for await (const event of runner.run({ sessionId: "textual-tool-call", goal: "use lookup" })) {
      events.push(event)
    }
    const persisted = await sessionLog.read("textual-tool-call")

    expect(persisted.some(entry => entry.event.kind === "llm_completed")).toBe(false)
    expect(events.find(event => event.type === "error")).toMatchObject({
      message: "Provider emitted a tool call as text instead of a native tool block",
    })
    expect(JSON.stringify({ events, persisted })).not.toContain("untrusted:")
    expect(JSON.stringify({ events, persisted })).not.toContain("secret")
  })
})
