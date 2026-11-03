/**
 * Plan task 1.1 · a workflow node's child run starts with the parent knowledge the kernel chose for
 * it by item scope (`inherited_knowledge` on the spawn), keyed and pinned as the parent held it.
 */
import { collectText, RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../src/types.js"

describe("inherited knowledge reaches the child run", () => {
  it("renders each inherited item in the child's knowledge", async () => {
    let seen = ""
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        seen = JSON.stringify(context)
        yield { type: "text_delta", delta: "done" }
      },
    }
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4096,
      maxTurns: 2,
      inheritedKnowledge: [{ content: "HOUSE_STYLE forbids bullet lists", key: "style", tokens: 6, pinned: true }],
    })
    await collectText(runner.run({ sessionId: "inherit", goal: "write the summary" }))
    expect(seen).toContain("HOUSE_STYLE forbids bullet lists")
  })

  it("reports which parent item each inherited copy came from, and which ones the child used", async () => {
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "Summary written in prose, as the HOUSE_STYLE forbids bullet lists." }
      },
    }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4096,
      maxTurns: 2,
      inheritedKnowledge: [
        { content: "HOUSE_STYLE forbids bullet lists", key: "style", tokens: 6, pinned: true,
          inherited_from: { operation: "op-parent", item: 4 } },
        { content: "orchid storage layout notes", key: "notes", tokens: 5,
          inherited_from: { operation: "op-parent", item: 5 } },
      ],
    })
    await collectText(runner.run({ sessionId: "inherit-edges", goal: "write the summary" }))
    const report = (await sessionLog.read("inherit-edges"))
      .map(entry => entry.event)
      .find(event => event.kind === "context_lifecycle_reported")
    expect(report?.kind).toBe("context_lifecycle_reported")
    if (report?.kind !== "context_lifecycle_reported") return
    expect(report.external_edges?.map(edge => [edge.relation, edge.to.item])).toEqual([
      ["inherited_from", 4],
      ["inherited_from", 5],
    ])
    expect(report.parent_items_used).toEqual([{ operation: "op-parent", item: 4 }])
  })
})
