import { jest } from "@jest/globals"
import {
  ProviderError,
  classifyProviderError,
  providerErrorEventFields,
} from "../src/providers/provider-error.js"
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { OllamaProvider } from "../src/providers/ollama.js"

class StructuredThrowingProvider implements LLMProvider {
  constructor(private readonly error: Error) {}

  async complete(): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  }

  // eslint-disable-next-line require-yield
  async *stream(): AsyncIterable<StreamEvent> {
    throw this.error
  }
}

async function runFailure(error: Error): Promise<StreamEvent[]> {
  const runner = new RuntimeRunner({
    provider: new StructuredThrowingProvider(error),
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 8_000,
    maxTurns: 3,
  } as never)
  const events: StreamEvent[] = []
  for await (const event of runner.run({ sessionId: "a08", goal: "x" })) events.push(event)
  return events
}

describe("SPC-013 A-08 structured provider failures", () => {
  it.each(["context_length_exceeded", "prompt_too_long"])(
    "classifies the precise %s provider code as context overflow",
    providerCode => {
      const cause = Object.assign(new Error("request rejected"), {
        status: 400,
        code: providerCode,
      })
      const error = classifyProviderError("openai", cause)
      expect(error).toMatchObject({
        provider: "openai",
        kind: "context_overflow",
        retryable: false,
        httpStatus: 400,
        providerCode,
      })
      expect(error.cause).toBe(cause)
    },
  )

  it("reads Anthropic's nested error code shape", () => {
    const error = classifyProviderError("anthropic", Object.assign(
      new Error("request rejected"),
      { status: 400, error: { error: { error_code: "prompt_too_long" } } },
    ))
    expect(error).toMatchObject({
      kind: "context_overflow",
      providerCode: "prompt_too_long",
      httpStatus: 400,
    })
  })

  it.each(["APIConnectionError", "APIConnectionTimeoutError"])(
    "classifies status-less SDK %s as transport",
    name => {
      const cause = Object.assign(new Error("connection failed"), { name })
      expect(classifyProviderError("openai", cause)).toMatchObject({
        kind: "transport",
        retryable: true,
        cause,
      })
    },
  )

  it("does not infer context overflow from a proxy 413", async () => {
    const error = new ProviderError({
      provider: "openai",
      kind: "unknown",
      retryable: false,
      httpStatus: 413,
      message: "HTTP 413: proxy request body is too long",
    })
    const events = await runFailure(error)
    expect(events.find(event => event.type === "done")).toMatchObject({ status: "error" })
  })

  it("uses structured context_overflow even when the message has no overflow wording", async () => {
    const error = new ProviderError({
      provider: "anthropic",
      kind: "context_overflow",
      retryable: false,
      httpStatus: 400,
      providerCode: "prompt_too_long",
      message: "request rejected",
    })
    const events = await runFailure(error)
    expect(events.find(event => event.type === "done")).toMatchObject({ status: "context_overflow" })
  })

  it("folds provider diagnostics into the closed canonical failure vocabulary", async () => {
    const events = await runFailure(new ProviderError({
      provider: "openai",
      kind: "auth",
      retryable: false,
      httpStatus: 401,
      providerCode: "invalid_api_key",
      message: "credentials rejected",
    }))
    expect(events.find(event => event.type === "done")).toMatchObject({ status: "error" })
  })

  it("projects only safe scalar fields into the host event", () => {
    const cause = Object.assign(new Error("secret SDK body"), { body: { raw: "secret" } })
    const error = new ProviderError({
      provider: "openai",
      kind: "rate_limit",
      retryable: true,
      httpStatus: 429,
      providerCode: "rate_limit",
      message: "slow down",
      cause,
    })
    expect(providerErrorEventFields(error)).toEqual({
      error_kind: "rate_limit",
      retryable: true,
      http_status: 429,
      provider_code: "rate_limit",
    })
    expect(providerErrorEventFields(error)).not.toHaveProperty("cause")
    expect(providerErrorEventFields(error)).not.toHaveProperty("stack")
    expect(providerErrorEventFields(error)).not.toHaveProperty("body")
  })

  it("does not serialize the retained SDK cause into runner events", async () => {
    const events = await runFailure(new ProviderError({
      provider: "openai",
      kind: "auth",
      retryable: false,
      message: "credentials rejected",
      cause: Object.assign(new Error("secret SDK response"), { body: { token: "secret" } }),
    }))
    const emitted = events.find(event => event.type === "error") as { message?: string } | undefined
    expect(emitted?.message).toBe("credentials rejected")
    expect(JSON.stringify(events)).not.toContain("secret SDK response")
    expect(JSON.stringify(events)).not.toContain("token")
  })

  it("keeps legacy message fallback for unwrapped third-party providers", async () => {
    const events = await runFailure(new Error("HTTP 413: prompt is too long"))
    expect(events.find(event => event.type === "done")).toMatchObject({ status: "context_overflow" })
  })

  it("wraps retry exhaustion while retaining the SDK cause", async () => {
    const cause = Object.assign(new Error("slow down"), { status: 429, code: "rate_limit" })
    const provider = new OpenAIChatProvider("k", "fixture", { maxRetries: 2, baseDelay: 0 })
    const create = jest.fn().mockRejectedValue(cause)
    ;(provider as any).client = { chat: { completions: { create } } }

    await expect(provider.complete({ systemText: "", turns: [] }, [])).rejects.toMatchObject({
      name: "ProviderError",
      provider: "openai",
      kind: "rate_limit",
      retryable: true,
      httpStatus: 429,
      cause,
    })
    expect(create).toHaveBeenCalledTimes(2)
  })

  it("wraps stream creation and iterator-pull failures", async () => {
    const creationCause = Object.assign(new Error("offline"), { code: "ECONNRESET" })
    const creation = new OpenAIChatProvider("k", "fixture", { maxRetries: 1, baseDelay: 0 })
    ;(creation as any).client = {
      chat: { completions: { create: jest.fn().mockRejectedValue(creationCause) } },
    }
    const creationIterator = creation.stream({ systemText: "", turns: [] }, [])[Symbol.asyncIterator]()
    await expect(creationIterator.next()).rejects.toMatchObject({
      name: "ProviderError",
      kind: "transport",
      cause: creationCause,
    })

    const pullCause = Object.assign(new Error("stream reset"), { code: "ECONNRESET" })
    const pull = new OpenAIChatProvider("k", "fixture", { maxRetries: 1, baseDelay: 0 })
    ;(pull as any).client = {
      chat: { completions: { create: async () => ({
        [Symbol.asyncIterator]() {
          return { next: async () => { throw pullCause } }
        },
      }) } },
    }
    const pullIterator = pull.stream({ systemText: "", turns: [] }, [])[Symbol.asyncIterator]()
    await expect(pullIterator.next()).rejects.toMatchObject({
      name: "ProviderError",
      kind: "transport",
      cause: pullCause,
    })
  })

  it("wraps HTTP non-ok and circuit-open boundaries", async () => {
    const originalFetch = globalThis.fetch
    globalThis.fetch = jest.fn().mockResolvedValue({ ok: false, status: 413 }) as typeof fetch
    try {
      const ollama = new OllamaProvider("fixture", "http://ollama.invalid")
      await expect(ollama.complete({ systemText: "", turns: [] }, [])).rejects.toMatchObject({
        name: "ProviderError",
        provider: "ollama",
        kind: "unknown",
        httpStatus: 413,
      })
    } finally {
      globalThis.fetch = originalFetch
    }

    const openai = new OpenAIChatProvider("k", "fixture", { maxRetries: 1, baseDelay: 0 })
    ;(openai as any).circuit = { isOpen: () => true }
    await expect(openai.complete({ systemText: "", turns: [] }, [])).rejects.toMatchObject({
      name: "ProviderError",
      kind: "model_unavailable",
      providerCode: "circuit_open",
      retryable: true,
    })
  })
})
