/**
 * O2 — `injectNote` (the system-reminder channel): an imperative host push into the run's signal
 * stream, without wiring a full `SignalSource`. A normal-urgency note is queued by the kernel
 * attention policy and drained at the next turn boundary, so it renders as a `[SIGNAL] <text>`
 * line in the state turn of the FOLLOWING provider call (same timing as a polled `signalSource`
 * signal — the in-flight turn's context is already rendered when the note is applied).
 */
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import type { RuntimeRunner } from "../src/runtime/runner.js"
import { createRunner, tool } from "./runtime/helpers.js"

class CapturingToolProvider implements LLMProvider {
  readonly calls: RenderedContext[] = []
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }
  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    this.calls.push(context)
    if (this.calls.length <= 2) {
      yield { type: "tool_call", id: `call_${this.calls.length}`, name: "set_title", arguments: { title: "same" } }
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

describe("injectNote (system-reminder channel)", () => {
  it("renders a host-injected note as a [SIGNAL] line after the next turn boundary", async () => {
    const provider = new CapturingToolProvider()
    let runnerRef: RuntimeRunner
    const setTitle = tool("set_title", "Set the document title", {
      type: "object",
      properties: { title: { type: "string" } },
      required: ["title"],
    }, ({ title }, _ctx) => {
      // Host-detected no-op write: feed precise negative feedback back to the model.
      runnerRef.injectNote(`title is already "${title}" — the write was a no-op, stop repeating it`)
      return "unchanged"
    })

    const { runner } = createRunner(provider, [setTitle], { maxTurns: 6 })
    runnerRef = runner
    for await (const _event of runner.run({ sessionId: "inject-note", goal: "set the title" })) { /* drain */ }

    expect(provider.calls.length).toBeGreaterThanOrEqual(3)
    // Turn 2's context was already rendered when the note from turn 1's tool run was applied;
    // the note must surface by turn 3's prompt.
    expect(renderedText(provider.calls[2])).toContain('[SIGNAL] title is already "same" — the write was a no-op, stop repeating it')
  })
})
