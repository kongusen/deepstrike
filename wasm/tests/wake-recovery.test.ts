/**
 * SessionLog-only wake must fail closed under canonical ABI v3 (Node wake-recovery parity).
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"
import { collectText } from "../src/runtime/index.js"

const provider: LLMProvider = {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "finished" }
  },
}

describe("RuntimeRunner wake recovery (wasm)", () => {
  it("does not continue from a SessionLog-only tool completion", async () => {
    const sessionLog = new InMemorySessionLog()
    const sessionId = "crash-test"
    await sessionLog.append(sessionId, {
      kind: "run_started", run_id: "r1", goal: "use ping", criteria: [],
    })
    await sessionLog.append(sessionId, {
      kind: "llm_completed",
      turn: 0,
      content: "",
      tool_calls: [{ id: "call_ping", name: "ping", arguments: "{}" }],
    })
    await sessionLog.append(sessionId, {
      kind: "tool_completed",
      turn: 0,
      results: [{ call_id: "call_ping", output: "pong", is_error: false }],
    })

    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 4,
    })

    await expect(collectText(runner.wake(sessionId))).rejects.toThrow(
      "restored canonical operation has no pending effect or terminal",
    )
  })
})
