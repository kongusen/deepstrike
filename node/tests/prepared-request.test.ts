import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import { prepareProviderRequest } from "../src/providers/prepared-request.js"
import { createProviderRequestPlanForProvider } from "../src/providers/request-plan.js"
import type { LLMProvider, RenderedContext } from "../src/types.js"

const message = { role: "assistant", content: "answer", toolCalls: [] }
const context: RenderedContext = { turns: [{ role: "user", content: "question" }, message] }
function fingerprint(provider: LLMProvider, prepared: ReturnType<typeof prepareProviderRequest>) {
  return createProviderRequestPlanForProvider(provider, context, [], {}, {
    scope: prepared.scope, request: prepared.request, state: prepared.state,
  }).fingerprint
}

test("Anthropic preparation binds hidden replay and sends the same body after counting", async () => {
  const provider = new AnthropicProvider({ apiKey: "test", model: "claude-sonnet-4-6" })
  const replayMessage = { ...message, toolCalls: [{ id: "call-1", name: "lookup", arguments: "{}" }] }
  const replayContext = { turns: [context.turns[0], replayMessage, { role: "tool", content: "result", toolCallId: "call-1" }] }
  const replay = (signature: string) => ({ protocol: "anthropic-messages" as const, native_blocks: [{ type: "thinking", thinking: "private", signature }, { type: "text", text: "answer" }, { type: "tool_use", id: "call-1", name: "lookup", input: {} }] })
  provider.seedProviderReplay(replayMessage, replay("original"))
  const prepared = prepareProviderRequest(provider, replayContext, [])
  const before = fingerprint(provider, prepared)
  provider.seedProviderReplay(replayMessage, replay("changed"))
  expect(fingerprint(provider, prepareProviderRequest(provider, replayContext, []))).not.toBe(before)
  let sent: unknown
  let counted: any
  const client = (provider as any).client
  client.messages.countTokens = async (body: any) => { counted = structuredClone(body); body.messages.length = 0; return { input_tokens: 12 } }
  client.messages.stream = async function* (body: unknown) { sent = body }
  await prepared.countTokens!()
  for await (const _ of prepared.stream()) { /* drain */ }
  const frozen = prepared.request as { body: any }
  expect(sent).toEqual(frozen.body)
  expect(counted.messages).toEqual(frozen.body.messages)
  expect(JSON.stringify(sent)).toContain("original")
  expect(JSON.stringify(sent)).not.toContain("changed")
})

test("Responses freezes continuation and updates the live state only from execution", async () => {
  const provider = new OpenAIResponsesProvider("test", "gpt-5.2")
  const state = { previousResponseId: "resp-original", coveredMessageCount: 1 }
  const prepared = prepareProviderRequest(provider, context, [], undefined, state)
  const original = fingerprint(provider, prepared)
  state.previousResponseId = "resp-mutated"
  expect(fingerprint(provider, prepareProviderRequest(provider, context, [], undefined, state))).not.toBe(original)
  let sent: any
  ;(provider as any).client.responses.create = async function* (body: any) {
    sent = body
    yield { type: "response.completed", response: { id: "resp-new", output: [], usage: { input_tokens: 1, output_tokens: 0 } } }
  }
  for await (const _ of prepared.stream()) { /* drain */ }
  expect(sent.previous_response_id).toBe("resp-original")
  expect(sent).toEqual(prepared.request)
  expect(state.previousResponseId).toBe("resp-new")
})

test("custom provider scope binds continuation and isolates counting mutations", async () => {
  let sent: any
  const provider: LLMProvider = {
    async complete() { return message },
    async countTokens(ctx) { ctx.turns.length = 0; return { inputTokens: 1, source: { kind: "heuristic" }, confidence: "low_confidence" } },
    async *stream(ctx, _tools, _options, state) { sent = { ctx, state: structuredClone(state) }; if (state) { state.next = 2; delete state.expired } },
  }
  const state = { next: 1, expired: true }
  const prepared = prepareProviderRequest(provider, context, [], {}, state)
  expect(prepared.scope).toBe("adapter_input")
  const original = fingerprint(provider, prepared)
  state.next = 9
  expect(fingerprint(provider, prepareProviderRequest(provider, context, [], {}, state))).not.toBe(original)
  await prepared.countTokens!()
  for await (const _ of prepared.stream()) { /* drain */ }
  expect(sent.ctx.turns).toHaveLength(2)
  expect(sent.state.next).toBe(1)
  expect(state.next).toBe(2)
  expect(state).not.toHaveProperty("expired")
})

test("custom request evidence excludes transport credentials while dispatch retains them", async () => {
  let options: unknown
  const provider: LLMProvider = {
    async complete() { return message },
    async *stream(_context, _tools, supplied) { options = supplied },
  }
  const prepared = prepareProviderRequest(provider, context, [], { apiKey: "credential-one", retry: 9, temperature: 0.5 })
  const changedTransport = prepareProviderRequest(provider, context, [], { apiKey: "credential-two", retry: 2, temperature: 0.5 })
  expect(JSON.stringify(prepared.request)).not.toContain("credential")
  expect(fingerprint(provider, prepared)).toBe(fingerprint(provider, changedTransport))
  for await (const _ of prepared.stream()) { /* drain */ }
  expect(options).toEqual({ apiKey: "credential-one", retry: 9, temperature: 0.5 })
})


test("custom fallback rejects state that JSON would silently discard or change", () => {
  const provider: LLMProvider = {
    async complete() { return { role: "assistant", content: "" } },
    async *stream() { /* no dispatch */ },
  }
  const cycle: Record<string, unknown> = {}; cycle.self = cycle
  const withSymbol = { [Symbol("hidden")]: 1 }
  const withGetter = Object.defineProperty({}, "value", { enumerable: true, get: () => 1 })
  const nonEnumerable = Object.defineProperty({}, "hidden", { value: 1 })
  const cases = [
    { value: undefined }, { value: () => 1 }, { value: Symbol("state") }, { value: 1n },
    { value: NaN }, { value: Infinity }, { value: -0 }, { value: new Map() },
    { value: new Set() }, { value: new Date() }, { value: [, 1] },
    cycle, withSymbol, withGetter, nonEnumerable,
  ]
  for (const state of cases) {
    expect(() => prepareProviderRequest(provider, context, [], undefined, state)).toThrow(/implement prepareRequest/)
  }
  expect(() => prepareProviderRequest(provider, context, [], { signal: new AbortController().signal }, { values: [null, true, 1, "value"] })).not.toThrow()
})
