// Plan task 9.0 · DeepSeek tool-call markup written as text on the OpenAI-chat protocol, against the
// fixture every SDK shares.
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import { openAIChatDialects, type OpenAIChatWireDialect } from "../src/providers/openai-chat-dialects.js"
import { ProtocolResponseError } from "../src/providers/protocol-adapter.js"
import type { CanonicalAdapterInput } from "../src/providers/content-normalization.js"

interface Case {
  name: string
  chunks: string[]
  text?: string
  calls?: Array<{ name: string; arguments: Record<string, unknown> }>
  error?: string
}

const fixture = JSON.parse(readFileSync(join(
  process.cwd(), "../tests/fixtures/provider-textual-tool-call/openai-chat.json",
), "utf8")) as { cases: Case[] }

function input(dialect: OpenAIChatWireDialect): CanonicalAdapterInput {
  return {
    context: { systemText: "system", turns: [] },
    tools: [{ name: "read_file", description: "Read", parameters: '{"type":"object"}' }],
    resolved: {
      identity: {
        providerId: dialect.providerId,
        modelId: "fixture-model",
        endpointId: dialect.endpointId,
        protocol: "openai-chat",
      },
    },
    extensions: {},
  } as unknown as CanonicalAdapterInput
}

function stream(dialect: OpenAIChatWireDialect, chunks: string[]) {
  const adapter = new OpenAIChatAdapter()
  const state = adapter.createStreamState({ input: input(dialect) }, dialect)
  const events = chunks.flatMap(content =>
    adapter.pushStreamChunk({ choices: [{ index: 0, delta: { content } }] } as never, state).events)
  adapter.pushStreamChunk({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] } as never, state)
  events.push(...adapter.finishStream(state).events)
  return {
    text: events.filter(event => event.type === "text_delta").map(event => (event as { delta: string }).delta).join(""),
    calls: events.filter(event => event.type === "tool_call")
      .map(event => event as { id: string; name: string; arguments: Record<string, unknown> }),
    replay: adapter.finishStream(state).replay as { tool_calls?: Array<{ id: string }> } | undefined,
  }
}

function complete(dialect: OpenAIChatWireDialect, content: string) {
  const adapter = new OpenAIChatAdapter()
  return adapter.decodeComplete({ choices: [{ message: { role: "assistant", content } }] }, { input: input(dialect) }, dialect)
}

function rejection(fn: () => unknown): ProtocolResponseError {
  try {
    fn()
  } catch (error) {
    expect(error).toBeInstanceOf(ProtocolResponseError)
    return error as ProtocolResponseError
  }
  throw new Error("expected a textual tool call rejection")
}

describe("plan task 9.0 · tool-call markup written as text (deepseek, openai-chat)", () => {
  const deepseek = openAIChatDialects.deepseek

  describe.each(fixture.cases.filter(c => !c.error))("$name", c => {
    it("streams: the markup never reaches the text and becomes tool calls", () => {
      const out = stream(deepseek, c.chunks)
      expect(out.text).toBe(c.text)
      expect(out.calls.map(({ name, arguments: args }) => ({ name, arguments: args }))).toEqual(c.calls)
      for (const call of out.calls) expect(call.id).toMatch(/^call_text_/)
      expect(new Set(out.calls.map(call => call.id)).size).toBe(out.calls.length)
      if (out.calls.length) {
        expect(out.replay?.tool_calls?.map(call => call.id)).toEqual(out.calls.map(call => call.id))
      }
    })

    it("completes: the same text and calls", () => {
      const { message } = complete(deepseek, c.chunks.join(""))
      expect(message.content).toBe(c.text)
      expect(message.toolCalls.map(({ name, arguments: args }) => ({
        name,
        arguments: typeof args === "string" ? JSON.parse(args) : args,
      }))).toEqual(c.calls)
    })
  })

  describe.each(fixture.cases.filter(c => c.error))("$name", c => {
    it("fails the call as a retryable protocol error without echoing the markup", () => {
      for (const fn of [() => stream(deepseek, c.chunks), () => complete(deepseek, c.chunks.join(""))]) {
        const error = rejection(fn)
        expect(error).toMatchObject({ providerCode: c.error, retryable: true })
        expect(error.message).not.toContain("logs/a.log")
      }
    })
  })

  it("other dialects pass the text through unchanged", () => {
    const text = fixture.cases[0].chunks.join("")
    const out = stream(openAIChatDialects.openai, fixture.cases[0].chunks)
    expect(out.text).toBe(text)
    expect(out.calls).toEqual([])
  })
})
