/**
 * Reactive recovery ladder (lifted into the kernel). The SDK runners no longer own the
 * classify + compact + retry + give-up policy for a provider context-overflow: they forward the
 * raw provider error to the kernel as `provider_error` and dispatch whatever action comes back.
 *
 * These e2e cases validate the NET-NEW runner wiring + the honest terminal + the withholding rule:
 *  - an unrecoverable overflow terminates as `context_overflow` (not the old fabricated `timeout`),
 *    and the raw provider error is surfaced because the kernel returned a terminal;
 *  - a non-overflow provider error terminates as `error`, also surfaced.
 *
 * The recover-then-retry DECISION (compact succeeds → `call_provider`, error withheld) is proven
 * deterministically by the kernel unit test `recover_overflow_compacts_and_retries` in
 * crates/deepstrike-core/src/scheduler/state_machine/tests.rs — it needs compactible history, which
 * is awkward to construct reliably from a fresh e2e run.
 */
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

class ThrowingProvider implements LLMProvider {
  calls = 0
  constructor(private readonly message: string) {}
  async complete(): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  }
  // eslint-disable-next-line require-yield
  async *stream(): AsyncIterable<StreamEvent> {
    this.calls += 1
    throw new Error(this.message)
  }
}

function makeRunner(provider: LLMProvider) {
  return new RuntimeRunner({
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 8000,
    maxTurns: 3,
  } as never)
}

describe("kernel-owned reactive recovery", () => {
  it("terminates an unrecoverable overflow as context_overflow and surfaces the error", async () => {
    const provider = new ThrowingProvider("HTTP 413: prompt is too long")
    const events: StreamEvent[] = []
    for await (const evt of makeRunner(provider).run({ sessionId: "overflow", goal: "x" })) {
      events.push(evt)
    }

    const done = events.find(e => e.type === "done") as { status?: string } | undefined
    // Honest terminal: the kernel's ContextOverflow, NOT the old SDK-fabricated `timeout`.
    expect(done?.status).toBe("context_overflow")
    // Withholding: the error surfaces because the kernel could not recover (returned a terminal).
    expect(events.some(e => e.type === "error")).toBe(true)
    // Single 413 with nothing compactible on a fresh run ⇒ kernel gives up immediately.
    expect(provider.calls).toBeGreaterThanOrEqual(1)
  })

  it("terminates a non-overflow provider error as error and surfaces it", async () => {
    const provider = new ThrowingProvider("HTTP 500 Internal Server Error")
    const events: StreamEvent[] = []
    for await (const evt of makeRunner(provider).run({ sessionId: "server-error", goal: "x" })) {
      events.push(evt)
    }

    const done = events.find(e => e.type === "done") as { status?: string } | undefined
    expect(done?.status).toBe("error")
    expect(events.some(e => e.type === "error")).toBe(true)
  })
})

/**
 * Phase 4: max-output-tokens recovery. A turn cut off at the output cap (provider stop_reason =
 * max_tokens/length) is no longer mistaken for a finished turn — the kernel keeps the partial,
 * nudges the model to resume, and re-calls (bounded). Drives the full SDK path: provider surfaces
 * stop_reason on the usage event → runner feeds it → kernel decides continue vs finish.
 */
class TruncatingProvider implements LLMProvider {
  calls = 0
  async complete(): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  }
  async *stream(): AsyncIterable<StreamEvent> {
    this.calls += 1
    yield { type: "text_delta", delta: `part${this.calls} ` } as StreamEvent
    // First turn is cut off at the cap; the second finishes cleanly.
    const stopReason = this.calls === 1 ? "max_tokens" : "end_turn"
    yield { type: "usage", totalTokens: 10, outputTokens: 5, stopReason } as StreamEvent
  }
}

describe("kernel-owned max-output-tokens recovery", () => {
  it("continues a truncated turn instead of finishing, then completes", async () => {
    const provider = new TruncatingProvider()
    const events: StreamEvent[] = []
    for await (const evt of makeRunner(provider).run({ sessionId: "truncated", goal: "write a lot" })) {
      events.push(evt)
    }

    // The truncation drove a continue: the provider was called again and the run finished cleanly.
    expect(provider.calls).toBe(2)
    const done = events.find(e => e.type === "done") as { status?: string } | undefined
    expect(done?.status).toBe("completed")
    // Both partials were streamed (the kernel kept the first and resumed).
    const text = events.filter(e => e.type === "text_delta").map(e => (e as { delta: string }).delta).join("")
    expect(text).toContain("part1")
    expect(text).toContain("part2")
  })
})
