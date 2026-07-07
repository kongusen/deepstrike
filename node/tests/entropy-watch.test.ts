/**
 * Session entropy — the kernel-side measurement behind a host "heartbeat entropy watch":
 * one `entropy_sample` stream event per completed turn (unconditional), plus the opt-in
 * `entropyWatch` threshold alert (`entropy_alert`), both mirrored into the session log.
 * `runner.latestEntropy()` is the pull companion for supervisors outside the stream.
 */
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import type { EntropyAlertEvent, EntropySampleEvent } from "../src/types.js"
import { createRunner, tool } from "./runtime/helpers.js"

/** Repeats the identical failing tool call `toolTurns` times, then finishes. */
class LoopingProvider implements LLMProvider {
  private turns = 0
  constructor(private readonly toolTurns: number) {}
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }
  async *stream(_context: RenderedContext): AsyncIterable<StreamEvent> {
    this.turns += 1
    if (this.turns <= this.toolTurns) {
      yield { type: "tool_call", id: `call_${this.turns}`, name: "poke", arguments: { same: true } }
      return
    }
    yield { type: "text_delta", delta: "done" }
  }
}

const failingPoke = () =>
  tool("poke", "Poke the thing", {
    type: "object",
    properties: { same: { type: "boolean" } },
  }, () => { throw new Error("still broken") })

describe("session entropy (heartbeat watch source)", () => {
  it("streams one entropy_sample per completed turn and exposes latestEntropy()", async () => {
    const { runner } = createRunner(new LoopingProvider(3), [failingPoke()], {
      maxTurns: 8,
      repeatFuse: false, // keep the loop alive long enough to observe samples
    })

    const samples: EntropySampleEvent[] = []
    for await (const event of runner.run({ sessionId: "entropy-sample", goal: "poke it" })) {
      if (event.type === "entropy_sample") samples.push(event as EntropySampleEvent)
    }

    expect(samples.length).toBeGreaterThanOrEqual(3)
    const last = samples[samples.length - 1].sample
    expect(last.scoreVersion).toBe(1)
    expect(last.failureRate).toBeCloseTo(1.0) // every poke threw
    expect(last.repeatPressure).toBe(0) // fuse off ⇒ repeat axis honestly reads 0
    expect(last.windowTurns).toBe(samples.length)
    expect(runner.latestEntropy()).toEqual(last)
  })

  it("emits entropy_alert only when the opt-in watch is armed, and logs both records", async () => {
    // Watch OFF: a disordered run never alerts.
    const off = createRunner(new LoopingProvider(3), [failingPoke()], { maxTurns: 8 })
    const offEvents: StreamEvent[] = []
    for await (const event of off.runner.run({ sessionId: "entropy-off", goal: "poke it" })) {
      offEvents.push(event)
    }
    expect(offEvents.some(e => e.type === "entropy_alert")).toBe(false)

    // Watch ON with a floor threshold: alerts exactly once while the score stays hot
    // (hysteresis disarms re-fires), and both event kinds reach the session log.
    const { runner, sessionLog } = createRunner(new LoopingProvider(4), [failingPoke()], {
      maxTurns: 10,
      entropyWatch: { threshold: 0.1, cooldownTurns: 0 },
    })
    const alerts: EntropyAlertEvent[] = []
    for await (const event of runner.run({ sessionId: "entropy-on", goal: "poke it" })) {
      if (event.type === "entropy_alert") alerts.push(event as EntropyAlertEvent)
    }
    expect(alerts.length).toBe(1)
    expect(alerts[0].threshold).toBeCloseTo(0.1)
    expect(alerts[0].score).toBeGreaterThan(0.1)

    const logged = await sessionLog.read("entropy-on")
    const kinds = logged.map(e => e.event.kind)
    expect(kinds).toContain("entropy_sample")
    expect(kinds).toContain("entropy_alert")
  })

  it("notifyModel feeds the model a durable [entropy] directive through the signal channel", async () => {
    class CapturingProvider extends LoopingProvider {
      readonly contexts: RenderedContext[] = []
      constructor() { super(3) }
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        this.contexts.push(context)
        yield* super.stream(context)
      }
    }
    const provider = new CapturingProvider()
    const { runner } = createRunner(provider, [failingPoke()], {
      maxTurns: 8,
      entropyWatch: { threshold: 0.1, cooldownTurns: 0, notifyModel: true },
    })
    for await (const _event of runner.run({ sessionId: "entropy-notify", goal: "poke it" })) { /* drain */ }

    const rendered = provider.contexts
      .map(c => [c.systemText, c.stateTurn?.content, ...c.turns.map(m => m.content)].filter(Boolean).join("\n"))
      .join("\n---\n")
    expect(rendered).toContain("[entropy]")
  })
})
