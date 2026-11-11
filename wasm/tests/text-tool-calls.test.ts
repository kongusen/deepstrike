/**
 * Plan task 9.0 · DeepSeek tool-call markup written as text, over the browser-safe fetch transport,
 * against the fixture every SDK shares. Mirrors node/tests/text-tool-calls.test.ts.
 */
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { OpenAIProvider, deepseek } from "../src/providers/openai.js"
import { ProviderError } from "../src/providers/provider-error.js"
import type { LLMProvider, RenderedContext, StreamEvent } from "../src/types.js"

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

function sse(chunks: string[]): string {
  const frames = chunks.map(content => ({ choices: [{ index: 0, delta: { content } }] }))
  frames.push({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] } as never)
  return frames.map(frame => `data: ${JSON.stringify(frame)}\n\n`).join("") + "data: [DONE]\n\n"
}

async function run(provider: LLMProvider, body: string): Promise<StreamEvent[]> {
  const original = globalThis.fetch
  globalThis.fetch = async () => new Response(body, { status: 200 })
  try {
    const context: RenderedContext = { systemText: "system", turns: [{ role: "user", content: "go" } as never] }
    const events: StreamEvent[] = []
    for await (const event of provider.stream(context, [])) events.push(event)
    return events
  } finally {
    globalThis.fetch = original
  }
}

const text = (events: StreamEvent[]) =>
  events.filter(event => event.type === "text_delta").map(event => (event as { delta: string }).delta).join("")
const calls = (events: StreamEvent[]) =>
  events.filter(event => event.type === "tool_call").map(event => event as { id: string; name: string; arguments: Record<string, unknown> })

describe("plan task 9.0 · tool-call markup written as text (deepseek)", () => {
  const provider = () => deepseek({ apiKey: "k" })

  it.each(fixture.cases.filter(c => !c.error).map(c => [c.name, c] as const))("%s", async (_name, c) => {
    const events = await run(provider(), sse(c.chunks))
    expect(text(events)).toBe(c.text)
    expect(calls(events).map(({ name, arguments: args }) => ({ name, arguments: args }))).toEqual(c.calls)
    for (const call of calls(events)) expect(call.id).toMatch(/^call_text_/)
  })

  it.each(fixture.cases.filter(c => c.error).map(c => [c.name, c] as const))("%s", async (_name, c) => {
    const error = await run(provider(), sse(c.chunks)).then(() => undefined, (cause: unknown) => cause)
    expect(error).toBeInstanceOf(ProviderError)
    expect(error).toMatchObject({ kind: "protocol", retryable: true, providerCode: c.error })
    expect((error as Error).message).not.toContain("logs/a.log")
  })

  it("other dialects pass the text through unchanged", async () => {
    const events = await run(new OpenAIProvider({ apiKey: "k" }), sse(fixture.cases[0].chunks))
    expect(text(events)).toBe(fixture.cases[0].chunks.join(""))
    expect(calls(events)).toEqual([])
  })

  it("native tool calls survive a stream that ends with [DONE]", async () => {
    const body = [
      { choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: "call_1", function: { name: "read_file", arguments: '{"path":"a"}' } }] } }] },
      { choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
    ].map(frame => `data: ${JSON.stringify(frame)}\n\n`).join("") + "data: [DONE]\n\n"
    const events = await run(provider(), body)
    expect(calls(events)).toEqual([{ type: "tool_call", id: "call_1", name: "read_file", arguments: { path: "a" } }])
  })
})
