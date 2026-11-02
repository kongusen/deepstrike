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
})
