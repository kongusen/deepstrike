/**
 * WASM runner parity for the kernel-owned reactive recovery ladder. The runner no longer owns the
 * classify + compact + retry + give-up policy: it forwards the raw provider error as
 * `provider_error` and dispatches the kernel's reply. The mock kernel (tests/__mocks__/kernel.ts)
 * mirrors the real ladder (bounded compact-and-retry, then honest ContextOverflow terminal).
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

function makeRunner(provider: LLMProvider) {
  return new RuntimeRunner({
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 2048,
  })
}

describe("kernel-owned reactive recovery (wasm runner wiring)", () => {
  it("recovers from a single overflow without leaking an error, then completes", async () => {
    let calls = 0
    const provider: LLMProvider = {
      async *stream(): AsyncIterable<StreamEvent> {
        calls += 1
        if (calls === 1) throw new Error("HTTP 413: prompt is too long")
        yield { type: "text_delta", delta: "ok" }
      },
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
    }
    const events: StreamEvent[] = []
    for await (const evt of makeRunner(provider).run({ sessionId: "recover", goal: "x" })) {
      events.push(evt)
    }

    const done = events.find(e => e.type === "done") as { status?: string } | undefined
    expect(done?.status).toBe("completed")
    // Withholding: a recovered turn must NOT leak an intermediate error event to embedders.
    expect(events.some(e => e.type === "error")).toBe(false)
    expect(calls).toBe(2)
  })

  it("terminates an unrecoverable overflow as context_overflow and surfaces the error", async () => {
    let calls = 0
    const provider: LLMProvider = {
      // eslint-disable-next-line require-yield
      async *stream(): AsyncIterable<StreamEvent> {
        calls += 1
        throw new Error("413 context_length_exceeded")
      },
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
    }
    const events: StreamEvent[] = []
    for await (const evt of makeRunner(provider).run({ sessionId: "overflow", goal: "x" })) {
      events.push(evt)
    }

    const done = events.find(e => e.type === "done") as { status?: string } | undefined
    // Honest terminal: ContextOverflow, not the old SDK-fabricated `timeout`.
    expect(done?.status).toBe("context_overflow")
    // Surfaced because the kernel could not recover (returned a terminal).
    expect(events.some(e => e.type === "error")).toBe(true)
    // Bounded ladder: 2 compact-and-retry attempts + the give-up.
    expect(calls).toBe(3)
  })
})
