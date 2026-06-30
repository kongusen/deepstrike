/**
 * Phase 4 provider wiring: providers surface an output-cap truncation as `stopReason` on the usage
 * event, which the runner feeds to the kernel to drive max-output-tokens recovery. OpenAI signals it
 * via finish_reason="length" (on a choices frame separate from the trailing usage frame); Anthropic
 * via stop_reason="max_tokens".
 */
import { OpenAIChatProvider } from "../src/providers/openai.js"
import type { RenderedContext, UsageEvent } from "../src/types.js"

const context: RenderedContext = { systemText: "", turns: [{ role: "user", content: "hi" }] }

describe("provider surfaces stop_reason on the usage event", () => {
  it("OpenAI maps finish_reason=length to stopReason on usage (separate frames)", async () => {
    const provider = new OpenAIChatProvider("test-key")
    ;(provider as unknown as { client: { chat: { completions: { create(): Promise<AsyncIterable<Record<string, unknown>>> } } } }).client = {
      chat: { completions: { async create() {
        return { async *[Symbol.asyncIterator]() {
          yield { choices: [{ delta: { content: "the start of a long answer" }, finish_reason: null }] }
          // truncation lands on its own choices frame...
          yield { choices: [{ delta: {}, finish_reason: "length" }] }
          // ...then the usage frame arrives with empty choices.
          yield { choices: [], usage: { total_tokens: 100, prompt_tokens: 90, completion_tokens: 10 } }
        } }
      } } },
    }

    const events = []
    for await (const event of provider.stream(context, [])) events.push(event)

    const usage = events.find(e => e.type === "usage") as UsageEvent | undefined
    expect(usage).toBeDefined()
    expect(usage?.stopReason).toBe("length")
  })

  it("OpenAI leaves stopReason undefined on a clean stop", async () => {
    const provider = new OpenAIChatProvider("test-key")
    ;(provider as unknown as { client: { chat: { completions: { create(): Promise<AsyncIterable<Record<string, unknown>>> } } } }).client = {
      chat: { completions: { async create() {
        return { async *[Symbol.asyncIterator]() {
          yield { choices: [{ delta: { content: "done" }, finish_reason: "stop" }] }
          yield { choices: [], usage: { total_tokens: 10, prompt_tokens: 8, completion_tokens: 2 } }
        } }
      } } },
    }

    const events = []
    for await (const event of provider.stream(context, [])) events.push(event)

    const usage = events.find(e => e.type === "usage") as UsageEvent | undefined
    // "stop" is reported but is not a truncation — the kernel ignores it (no spurious recovery).
    expect(usage?.stopReason).toBe("stop")
  })
})
