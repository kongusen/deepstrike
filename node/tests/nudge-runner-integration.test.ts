/**
 * Self-Harness H1.2 — nudge → runner wiring.
 *
 * The mechanism's only causal channel is the bytes of the NEXT provider request. This test drives a
 * scripted provider + a throwing tool: after the tool error, the configured nudge note must appear as
 * a `[SIGNAL]` line in a SUBSEQUENT provider request (asserted on the context the provider captured).
 * The control proves the promised zero-behavior-difference: a run with no matching nudge yields a
 * session event stream byte-identical to the no-nudges baseline and injects no signal.
 */
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import type { SessionEvent } from "../src/runtime/session-log.js"
import { createRunner, tool } from "./runtime/helpers.js"

class CapturingProvider implements LLMProvider {
  readonly calls: RenderedContext[] = []
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }
  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    this.calls.push(context)
    if (this.calls.length <= 3) {
      yield { type: "tool_call", id: `call_${this.calls.length}`, name: "flaky", arguments: {} }
      return
    }
    yield { type: "text_delta", delta: "done" }
  }
}

function renderedText(ctx: RenderedContext): string {
  return [ctx.systemText, ctx.systemStable, ctx.systemKnowledge, ctx.stateTurn?.content, ...ctx.turns.map(m => m.content)]
    .filter(Boolean)
    .join("\n")
}

const flakyTool = () =>
  tool("flaky", "A tool that always fails", { type: "object", properties: {} }, () => {
    throw new Error("kaboom")
  })

/** run_started carries a random run_id; normalize it so two runs are comparable. */
function normalize(events: SessionEvent[]): SessionEvent[] {
  return events.map(e => (e.kind === "run_started" ? { ...e, run_id: "<run>" } : e))
}

const NOTE = "the flaky tool failed — try a different approach"

describe("nudge → runner integration", () => {
  it("delivers a tool_error nudge note into a subsequent provider request", async () => {
    const provider = new CapturingProvider()
    const { runner } = createRunner(provider, [flakyTool()], {
      maxTurns: 6,
      nudges: [{ id: "on-err", on: { kind: "tool_error" }, note: NOTE }],
    })
    for await (const _event of runner.run({ sessionId: "nudge-int", goal: "call flaky" })) { /* drain */ }

    // The first request precedes the error, so it must be clean; the note must reach a later request.
    expect(renderedText(provider.calls[0])).not.toContain("[SIGNAL]")
    const withSignal = provider.calls.findIndex(c => renderedText(c).includes(`[SIGNAL] ${NOTE}`))
    expect(withSignal).toBeGreaterThanOrEqual(1)
  })

  it("without a firing nudge the event stream equals the baseline and injects no signal", async () => {
    const runOnce = async (sessionId: string, nudges?: Parameters<typeof createRunner>[2]["nudges"]) => {
      const provider = new CapturingProvider()
      const { runner, sessionLog } = createRunner(provider, [flakyTool()], { maxTurns: 6, ...(nudges ? { nudges } : {}) })
      for await (const _event of runner.run({ sessionId, goal: "call flaky" })) { /* drain */ }
      return {
        events: (await sessionLog.read(sessionId)).map(e => e.event),
        contexts: provider.calls.map(renderedText),
      }
    }

    const baseline = await runOnce("base")
    // A never-firing rule (no tool_denied ever occurs) still wraps the append funnel, so this proves
    // the wrapping is side-effect-free when nothing fires — not merely that the option is ignored.
    const neverFires = await runOnce("never", [{ id: "denied-only", on: { kind: "tool_denied" }, note: "never" }])

    expect(normalize(neverFires.events)).toEqual(normalize(baseline.events))
    for (const ctx of [...baseline.contexts, ...neverFires.contexts]) expect(ctx).not.toContain("[SIGNAL]")
  })
})
