import { InMemorySessionLog, LocalExecutionPlane, RuntimeRunner } from "../src/runtime/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

describe("kernel cancellation transaction", () => {
  it.each(["user", "deadline", "lease_lost", "host_shutdown"] as const)(
    "commits the %s reason with its pending provider call",
    async reason => {
      const provider: LLMProvider = {
        async complete(): Promise<Message> {
          return { role: "assistant", content: "", toolCalls: [] }
        },
        async *stream(): AsyncIterable<StreamEvent> {
          yield { type: "text_delta", delta: "first" }
          for (let i = 0; i < 1000; i += 1) yield { type: "text_delta", delta: "later" }
        },
      }
      const sessionLog = new InMemorySessionLog()
      const runner = new RuntimeRunner({ provider, sessionLog, executionPlane: new LocalExecutionPlane(), maxTokens: 2048 })

      for await (const event of runner.run({ sessionId: `cancel-${reason}`, goal: "cancel me" })) {
        if (event.type === "text_delta") runner.interrupt(reason)
      }

      const cancellation = (await sessionLog.read(`cancel-${reason}`))
        .map(entry => entry.event)
        .find(event => event.kind === "operation_cancelled")
      expect(cancellation).toMatchObject({ kind: "operation_cancelled", reason })
      expect(cancellation && "pending_call_ids" in cancellation ? cancellation.pending_call_ids : []).toHaveLength(1)
    },
  )
})
