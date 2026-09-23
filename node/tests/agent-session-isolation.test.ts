import { createAgent } from "../src/index.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

class ControlledProvider implements LLMProvider {
  readonly signals: AbortSignal[] = []
  private startedResolve!: () => void
  private startedCount = 0
  readonly started = new Promise<void>(resolve => { this.startedResolve = resolve })

  async complete(): Promise<ModelMessage> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }

  async *stream(
    _context: RenderedContext,
    _tools: ToolSchema[],
    _extensions?: Record<string, unknown>,
    _state?: unknown,
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    this.signals.push(signal!)
    this.startedCount += 1
    if (this.startedCount >= 2) this.startedResolve()
    yield { type: "text_delta", delta: `call-${this.startedCount}` }
    while (!signal?.aborted) await new Promise(resolve => setTimeout(resolve, 1))
  }
}

describe("Agent facade session isolation", () => {
  it("interrupts only the targeted session while another session remains active", async () => {
    const provider = new ControlledProvider()
    const agent = createAgent({ runtimeBinding: { provider } })
    const first = agent.run("first", { session: { id: "session-first" } })
    const second = agent.run("second", { session: { id: "session-second" } })
    await provider.started

    agent.session("session-first").interrupt("user")
    const firstResult = await first
    expect(firstResult).toMatchObject({ sessionId: "session-first", status: "cancelled" })

    let secondSettled = false
    void second.then(() => { secondSettled = true })
    await new Promise(resolve => setTimeout(resolve, 10))
    expect(secondSettled).toBe(false)

    agent.session("session-second").interrupt("user")
    await expect(second).resolves.toMatchObject({ sessionId: "session-second", status: "cancelled" })
    expect(provider.signals).toHaveLength(2)
    expect(provider.signals.every(signal => signal.aborted)).toBe(true)
  })

  it("rejects overlapping runs admitted to the same session", async () => {
    const provider = new ControlledProvider()
    const agent = createAgent({ runtimeBinding: { provider } })
    const first = agent.run("first", { session: { id: "same-session" } })
    await new Promise(resolve => setTimeout(resolve, 5))

    await expect(agent.run("second", { session: { id: "same-session" } })).rejects.toThrow(/already has an active run/i)
    agent.session("same-session").interrupt("user")
    await expect(first).resolves.toMatchObject({ status: "cancelled" })
  })
})
