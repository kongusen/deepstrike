import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { FileSessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { AnthropicProvider } from "../../src/providers/anthropic.js"
import { tool } from "../../src/tools/index.js"
import type { RenderedContext, StreamEvent, ToolSchema, Message } from "../../src/types.js"

class CapturingAnthropicProvider extends AnthropicProvider {
  streamCalls = 0
  capturedRequest?: Record<string, unknown>

  constructor() {
    super("test-key")
    ;(this as unknown as {
      client: { messages: { stream(req: Record<string, unknown>): AsyncIterable<Record<string, unknown>> } }
    }).client = {
      messages: {
        stream: (req: Record<string, unknown>) => {
          this.streamCalls += 1
          if (this.streamCalls === 1) this.capturedRequest = req
          return this.mockStream()
        },
      },
    }
  }

  private async *mockStream(): AsyncIterable<Record<string, unknown>> {
    yield { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }
    yield { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "finished" } }
    yield { type: "content_block_stop", index: 0 }
  }
}

describe("RuntimeRunner thinking wake recovery", () => {
  it("restores thinking blocks from provider_replay after a new provider instance wakes", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-thinking-wake-"))
    try {
      const sessionId = "thinking-wake"
      const sessionLog = new FileSessionLog(dir)

      await sessionLog.append(sessionId, {
        kind: "run_started",
        run_id: "r1",
        goal: "use ping",
        criteria: [],
      })
      await sessionLog.append(sessionId, {
        kind: "llm_completed",
        turn: 0,
        content: "checking",
        tool_calls: [{ id: "call_ping", name: "ping", arguments: "{}" }],
        provider_replay: {
          native_blocks: [
            { type: "thinking", thinking: "plan", signature: "sig" },
            { type: "text", text: "checking" },
            { type: "tool_use", id: "call_ping", name: "ping", input: {} },
          ],
        },
      })
      await sessionLog.append(sessionId, {
        kind: "tool_completed",
        turn: 0,
        results: [{ call_id: "call_ping", output: "pong", is_error: false }],
      })

      const provider = new CapturingAnthropicProvider()
      const runner = new RuntimeRunner({
        provider,
        sessionLog: new FileSessionLog(dir),
        executionPlane: new LocalExecutionPlane().register(
          tool("ping", "Ping", { type: "object", properties: {} }, () => "should-not-run"),
        ),
        maxTokens: 2048,
        maxTurns: 4,
      })

      const text = await collectText(runner.wake(sessionId))
      expect(text).toBe("finished")
      expect(provider.streamCalls).toBe(1)
      expect(provider.capturedRequest?.messages).toEqual([
        { role: "user", content: [{ type: "text", text: "use ping", cache_control: { type: "ephemeral" } }] },
        {
          role: "assistant",
          content: [
            { type: "thinking", thinking: "plan", signature: "sig" },
            { type: "text", text: "checking" },
            { type: "tool_use", id: "call_ping", name: "ping", input: {} },
          ],
        },
        {
          role: "user",
          content: [{ type: "tool_result", tool_use_id: "call_ping", content: "pong", is_error: false, cache_control: { type: "ephemeral" } }],
        },
      ])
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })
})
