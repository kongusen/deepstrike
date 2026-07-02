/**
 * O5 — host tool hooks (the PreToolUse/PostToolUse-hook analog):
 *  - `onToolCall` gets name+args for each kernel-approved call and can BLOCK it with a reason;
 *    the call never executes and the reason reaches the model as a denied tool result.
 *  - `onToolResult` sees each executed result and can replace the output and/or inject a note
 *    into the signal stream (the `injectNote` channel).
 * This is the seam for STATEFUL host policy — e.g. counting repeats — while `governancePolicy`
 * stays static/declarative (the Claude Code rules-vs-hooks split).
 */
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import { createRunner, tool } from "./runtime/helpers.js"

class TwoToolTurnsProvider implements LLMProvider {
  readonly calls: RenderedContext[] = []
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }
  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    this.calls.push(context)
    if (this.calls.length <= 2) {
      yield { type: "tool_call", id: `call_${this.calls.length}`, name: "write_thing", arguments: { v: "x" } }
      return
    }
    yield { type: "text_delta", delta: "done" }
  }
}

function makeWriteTool(executed: string[]) {
  return tool("write_thing", "Write", {
    type: "object",
    properties: { v: { type: "string" } },
    required: ["v"],
  }, ({ v }) => {
    executed.push(String(v))
    return "written"
  })
}

describe("onToolCall (pre-tool hook)", () => {
  it("blocks a call statefully and feeds the reason back to the model", async () => {
    const provider = new TwoToolTurnsProvider()
    const executed: string[] = []
    const seen = new Map<string, number>()
    const { runner } = createRunner(provider, [makeWriteTool(executed)], { maxTurns: 6 })
    // Stateful policy the declarative rules can't express: deny the 2nd identical call.
    ;(runner as unknown as { opts: { onToolCall: unknown } }).opts.onToolCall = (
      call: { name: string; arguments: string },
    ) => {
      const key = `${call.name}:${call.arguments}`
      const n = (seen.get(key) ?? 0) + 1
      seen.set(key, n)
      if (n >= 2) return { block: true, reason: "duplicate call — do something different" }
    }

    const events: StreamEvent[] = []
    for await (const evt of runner.run({ sessionId: "hook-block", goal: "write" })) events.push(evt)

    // Executed exactly once; the duplicate was vetoed before execution.
    expect(executed).toEqual(["x"])
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_denied",
      toolName: "write_thing",
      reason: "duplicate call — do something different",
    }))
    // The reason reaches the model: the turn after the block carries the governance note.
    const later = provider.calls.slice(2).map(c =>
      [c.systemText, c.stateTurn?.content, ...c.turns.map(m => m.content)].filter(Boolean).join("\n"),
    ).join("\n")
    expect(later).toContain("duplicate call — do something different")
  })
})

describe("onToolResult (post-tool hook)", () => {
  it("replaces the output and injects a note into the signal stream", async () => {
    const provider = new TwoToolTurnsProvider()
    const executed: string[] = []
    const { runner } = createRunner(provider, [makeWriteTool(executed)], { maxTurns: 6 })
    ;(runner as unknown as { opts: { onToolResult: unknown } }).opts.onToolResult = (
      r: { output: string },
    ) => {
      if (r.output === "written") {
        return { replaceOutput: "written (no change detected)", note: "the write was a no-op" }
      }
    }

    for await (const _evt of runner.run({ sessionId: "hook-result", goal: "write" })) { /* drain */ }

    expect(executed.length).toBeGreaterThanOrEqual(1)
    // Tool outputs live in contentParts (tool_result parts), not message.content — stringify whole turns.
    const all = provider.calls.map(c =>
      [c.systemText, c.stateTurn?.content, JSON.stringify(c.turns)].filter(Boolean).join("\n"),
    ).join("\n")
    expect(all).toContain("written (no change detected)")
    expect(all).toContain("[SIGNAL] the write was a no-op")
  })
})
