import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIProvider } from "../src/providers/openai.js"
import { prepareProviderRequest } from "../src/providers/prepared-request.js"
import { createProviderRequestPlanForProvider } from "../src/providers/request-plan.js"
import type { LLMProvider, PreparedProviderRequest, RenderedContext } from "../src/types.js"

const context: RenderedContext = { turns: [{ role: "user", content: "question" }] }
function fingerprint(provider: LLMProvider, prepared: PreparedProviderRequest) {
  return createProviderRequestPlanForProvider(provider, context, [], {}, { scope: prepared.scope, request: prepared.request, state: prepared.state }).fingerprint
}

test("Anthropic snapshot freezes hidden native replay before fetch", async () => {
  const provider = new AnthropicProvider("test", "claude-sonnet-4-6")
  const message = { role: "assistant", content: "answer", toolCalls: [{ id: "call-1", name: "lookup", arguments: "{}" }] }
  const ctx = { turns: [context.turns[0], message, { role: "tool", content: "result", toolCallId: "call-1" }] }
  const replay = (signature: string) => ({ protocol: "anthropic-messages" as const, native_blocks: [{ type: "thinking", thinking: "private", signature }, { type: "tool_use", id: "call-1", name: "lookup", input: {} }] })
  provider.seedProviderReplay(message, replay("original"))
  const prepared = prepareProviderRequest(provider, ctx, [])
  provider.seedProviderReplay(message, replay("changed"))
  expect(fingerprint(provider, prepareProviderRequest(provider, ctx, []))).not.toBe(fingerprint(provider, prepared))
  const originalFetch = globalThis.fetch
  let sent: unknown
  globalThis.fetch = async (_url, init) => { sent = JSON.parse(String(init?.body)); return new Response("", { status: 200 }) }
  try { for await (const _ of prepared.stream()) { /* drain */ } } finally { globalThis.fetch = originalFetch }
  expect(sent).toEqual(prepared.request)
  expect(JSON.stringify(sent)).toContain("original")
  expect(JSON.stringify(sent)).not.toContain("changed")
})

test("OpenAI preparation freezes dialect options and exact dispatched body", async () => {
  const provider = new OpenAIProvider({ apiKey: "test", model: "test", dialect: "qwen" })
  const options = { enableThinking: true, thinkingBudget: 42 }
  const prepared = prepareProviderRequest(provider, context, [], options)
  options.thinkingBudget = 99
  const originalFetch = globalThis.fetch
  let sent: unknown
  globalThis.fetch = async (_url, init) => { sent = JSON.parse(String(init?.body)); return new Response("", { status: 200 }) }
  try { for await (const _ of prepared.stream()) { /* drain */ } } finally { globalThis.fetch = originalFetch }
  expect(sent).toEqual(prepared.request)
  expect(sent).toMatchObject({ thinking_budget: 42 })
})

test("custom adapter input fingerprints and executes frozen continuation, including deletion", async () => {
  let dispatched: unknown
  const provider: LLMProvider = {
    async complete() { return { role: "assistant", content: "" } },
    async *stream(_context, _tools, _options, state) { dispatched = { ...state }; if (state) { delete state.expired; state.next = 2 } },
  }
  const state = { next: 1, expired: true }
  const prepared = prepareProviderRequest(provider, context, [], undefined, state)
  state.next = 9
  expect(prepared.scope).toBe("adapter_input")
  expect(fingerprint(provider, prepared)).not.toBe(fingerprint(provider, prepareProviderRequest(provider, context, [], undefined, state)))
  for await (const _ of prepared.stream()) { /* drain */ }
  expect(dispatched).toEqual({ next: 1, expired: true })
  expect(state).toEqual({ next: 2 })
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
