/**
 * Plan task 3.1 · a workflow node's child run holds its dependencies' outputs in the kernel and
 * reads one in full with `read_result(call_id="dep:<agent_id>")`, with no payload store involved.
 */
import { collectText, RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../src/types.js"

describe("dependency outputs are read through read_result", () => {
  it("advertises read_result and answers a dep: read with the full output", async () => {
    let turn = 0
    let tools: string[] = []
    let seen = ""
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(context: RenderedContext, offered): AsyncIterable<StreamEvent> {
        turn += 1
        if (turn === 1) {
          tools = (offered ?? []).map(tool => tool.name)
          yield { type: "tool_call", id: "r1", name: "read_result", arguments: { call_id: "dep:wf-node0" } }
          return
        }
        seen = JSON.stringify(context.turns)
        yield { type: "text_delta", delta: "done" }
      },
    }
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4096,
      maxTurns: 4,
      dependencyOutputs: { "wf-node0": "FULL_SURVEY_BODY with every finding" },
    })
    await collectText(runner.run({ sessionId: "dep-read", goal: "synthesize" }))
    expect(tools).toContain("read_result")
    expect(seen).toContain("[PAYLOAD handle_id=dep:wf-node0]")
    expect(seen).toContain("FULL_SURVEY_BODY with every finding")
  })
})
