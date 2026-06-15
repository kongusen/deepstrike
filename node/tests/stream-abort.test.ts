/**
 * #2-B-ii (foundation): `interrupt()` aborts the in-flight provider stream. The runner threads an
 * AbortSignal into `provider.stream(..., signal)` and breaks the consume loop on abort, so a preempt
 * (or any interrupt) cancels a live LLM call immediately instead of waiting for it to finish.
 */
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

class LongStreamProvider implements LLMProvider {
  received: AbortSignal | undefined
  async complete(): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  }
  async *stream(_c: RenderedContext, _t: ToolSchema[], _e?: Record<string, unknown>, _s?: unknown, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    this.received = signal
    yield { type: "text_delta", delta: "hi" }
    // Simulate a long in-flight call: keep streaming until the signal is aborted (honors the signal).
    let n = 0
    while (!signal?.aborted && n < 1000) {
      n += 1
      await Promise.resolve()
      yield { type: "text_delta", delta: "x" }
    }
  }
}

describe("#2-B-ii interrupt aborts the in-flight stream", () => {
  it("propagates an AbortSignal to provider.stream and stops the live call on interrupt()", async () => {
    const provider = new LongStreamProvider()
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      maxTurns: 3,
    } as never)

    let count = 0
    for await (const evt of runner.run({ sessionId: "abort", goal: "stream forever" })) {
      if (evt.type === "text_delta") {
        count += 1
        if (count === 1) runner.interrupt() // preempt mid-stream after the first token
      }
    }

    // The signal reached the provider, and the in-flight stream was aborted (didn't run to the cap).
    expect(provider.received).toBeDefined()
    expect(provider.received?.aborted).toBe(true)
    expect(count).toBeLessThan(1000)
  })
})
