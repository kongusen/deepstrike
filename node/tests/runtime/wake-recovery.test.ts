import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { FileSessionLog, InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { tool } from "../../src/tools/index.js"
import { createRunner } from "./helpers.js"
import type { LLMProvider, RenderedContext, StreamEvent, ToolSchema, Message } from "../../src/types.js"

/** Emits a tool call only when history has no tool results yet. */
class ResumeAwareProvider implements LLMProvider {
  streamCalls = 0

  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }

  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    this.streamCalls += 1
    const hasToolResult = context.turns.some(t => t.role === "tool")
    if (!hasToolResult) {
      yield { type: "tool_call", id: "call_ping", name: "ping", arguments: {} }
      return
    }
    yield { type: "text_delta", delta: "finished" }
  }
}

describe("RuntimeRunner wake recovery", () => {
  it("wake executes pending tools when stopped after llm_completed", async () => {
    let pingExecutions = 0
    const provider = new ResumeAwareProvider()
    const { runner, sessionLog } = createRunner(
      provider,
      [tool("ping", "Ping", { type: "object", properties: {} }, () => {
        pingExecutions += 1
        return "pong"
      })],
      { maxTurns: 4 },
    )

    const sessionId = "pending-tools"
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
    })

    const text = await collectText(runner.wake(sessionId))
    expect(text).toBe("finished")
    expect(pingExecutions).toBe(1)
    const after = await sessionLog.read(sessionId)
    expect(after.some(e => e.event.kind === "tool_completed")).toBe(true)
    expect(after.some(e => e.event.kind === "run_terminal")).toBe(true)
  })

  it("wake continues after tool_completed without re-running the tool", async () => {
    let pingExecutions = 0
    const provider = new ResumeAwareProvider()
    const { runner, sessionLog } = createRunner(
      provider,
      [tool("ping", "Ping", { type: "object", properties: {} }, () => {
        pingExecutions += 1
        return "pong"
      })],
      { maxTurns: 4 },
    )

    const sessionId = "crash-test"
    // Simulate kill after tool_completed but before run_terminal (no live run needed).
    await sessionLog.append(sessionId, {
      kind: "run_started",
      run_id: "r1",
      goal: "use ping then finish",
      criteria: [],
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

    const text = await collectText(runner.wake(sessionId))
    expect(text).toBe("finished")
    expect(pingExecutions).toBe(0)
    expect(provider.streamCalls).toBe(1)

    const after = await sessionLog.read(sessionId)
    expect(after.some(e => e.event.kind === "run_terminal")).toBe(true)
  })

  it("wake is a no-op when session already has run_terminal", async () => {
    const provider: LLMProvider = {
      async complete() { return { role: "assistant", content: "done", toolCalls: [] } },
      async *stream() { yield { type: "text_delta", delta: "done" } },
    }
    const { runner, sessionLog } = createRunner(provider, [], { maxTurns: 2 })
    const sessionId = "done-session"
    await collectText(runner.run({ sessionId, goal: "hello" }))

    const events: StreamEvent[] = []
    for await (const evt of runner.wake(sessionId)) events.push(evt)
    expect(events).toHaveLength(0)
    expect(await sessionLog.latestSeq(sessionId)).toBe(
      (await sessionLog.read(sessionId)).length - 1,
    )
  })

  it("FileSessionLog wake survives a new runner instance (process restart)", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-wake-"))
    try {
      const sessionId = "file-wake"
      const sessionLog1 = new FileSessionLog(dir)
      const plane1 = new LocalExecutionPlane()
      plane1.register(tool("ping", "Ping", { type: "object", properties: {} }, () => "pong"))
      const runner1 = new RuntimeRunner({
        provider: new ResumeAwareProvider(),
        sessionLog: sessionLog1,
        executionPlane: plane1,
        maxTokens: 2048,
        maxTurns: 4,
      })

      await sessionLog1.append(sessionId, {
        kind: "run_started",
        run_id: "r1",
        goal: "ping once",
        criteria: [],
      })
      await sessionLog1.append(sessionId, {
        kind: "llm_completed",
        turn: 0,
        content: "",
        tool_calls: [{ id: "call_ping", name: "ping", arguments: "{}" }],
      })
      await sessionLog1.append(sessionId, {
        kind: "tool_completed",
        turn: 0,
        results: [{ call_id: "call_ping", output: "pong", is_error: false }],
      })

      const sessionLog2 = new FileSessionLog(dir)
      const plane2 = new LocalExecutionPlane()
      plane2.register(tool("ping", "Ping", { type: "object", properties: {} }, () => "should-not-run"))
      const provider2 = new ResumeAwareProvider()
      const runner2 = new RuntimeRunner({
        provider: provider2,
        sessionLog: sessionLog2,
        executionPlane: plane2,
        maxTokens: 2048,
        maxTurns: 4,
      })

      const text = await collectText(runner2.wake(sessionId))
      expect(text).toBe("finished")
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })

  it("second run on same session preloads prior transcript", async () => {
    class CapturingProvider implements LLMProvider {
      readonly calls: RenderedContext[] = []
      async complete() { return { role: "assistant", content: "unused", toolCalls: [] } }
      async *stream(context: RenderedContext) {
        this.calls.push(context)
        yield { type: "text_delta", delta: `answer-${this.calls.length}` }
      }
    }

    const provider = new CapturingProvider()
    const { runner } = createRunner(provider, [], { maxTokens: 2048 })
    const sessionId = "continuity"

    await collectText(runner.run({ sessionId, goal: "My name is Ada." }))
    await collectText(runner.run({ sessionId, goal: "What is my name?" }))

    expect(provider.calls[1].turns).toEqual(expect.arrayContaining([
      expect.objectContaining({ role: "user", content: "My name is Ada." }),
      expect.objectContaining({ role: "assistant", content: "answer-1" }),
    ]))
    const ctx = provider.calls[1]
    const allContent = [ctx.stateTurn, ...ctx.turns].filter(Boolean).map(m => m!.content).join("\n")
    expect(allContent).toContain("What is my name?")
  })

  it("records compressed events when kernel compresses context", async () => {
    let toolRuns = 0
    class CompressionProvider implements LLMProvider {
      streamCalls = 0
      async complete() { return { role: "assistant" as const, content: "unused", toolCalls: [] } }
      async *stream() {
        this.streamCalls += 1
        if (this.streamCalls === 1) {
          yield { type: "tool_call", id: "call_ping", name: "ping", arguments: {} } as StreamEvent
          return
        }
        yield { type: "text_delta", delta: "finished" } as StreamEvent
      }
    }
    const provider = new CompressionProvider()
    const { runner, sessionLog } = createRunner(
      provider,
      [tool("ping", "Ping", { type: "object", properties: {} }, () => {
        toolRuns += 1
        return "pong ".repeat(200)
      })],
      { maxTokens: 32, maxTurns: 4 },
    )

    await collectText(runner.run({ sessionId: "compressed-session", goal: "use ping then finish" }))

    const events = await sessionLog.read("compressed-session")
    const compressed = events.find(e => e.event.kind === "compressed")
    expect(toolRuns).toBe(1)
    expect(compressed?.event).toEqual(expect.objectContaining({
      kind: "compressed",
      archived_seq_range: expect.any(Array),
    }))
    expect((compressed!.event as { archived_seq_range: [number, number] }).archived_seq_range[0]).toBe(0)
  })

  it("reactively compacts and retries once when provider reports prompt too long", async () => {
    class TooLongThenOkProvider implements LLMProvider {
      streamCalls = 0
      async complete() { return { role: "assistant" as const, content: "unused", toolCalls: [] } }
      async *stream() {
        this.streamCalls += 1
        if (this.streamCalls === 1) throw new Error("413 prompt too long")
        yield { type: "text_delta", delta: "recovered" } as StreamEvent
      }
    }

    const provider = new TooLongThenOkProvider()
    const { runner, sessionLog } = createRunner(provider, [], {
      maxTokens: 1000,
      maxTurns: 4,
    })
    const sessionId = "reactive-compact"
    await sessionLog.append(sessionId, {
      kind: "run_started",
      run_id: "seed",
      goal: "seed ".repeat(1200),
      criteria: [],
    })
    await sessionLog.append(sessionId, {
      kind: "llm_completed",
      turn: 0,
      content: "prior answer ".repeat(400),
      tool_calls: [],
    })
    await sessionLog.append(sessionId, {
      kind: "run_terminal",
      reason: "completed",
      turns_used: 1,
      total_tokens: 0,
    })

    const text = await collectText(runner.run({
      sessionId,
      goal: "a".repeat(5000),
    }))

    expect(text).toBe("recovered")
    expect(provider.streamCalls).toBe(2)
    const events = await sessionLog.read(sessionId)
    expect(events.some(e => e.event.kind === "compressed")).toBe(true)
  })

  it("recoverable tool failure preserves replay context", async () => {
    let callCount = 0
    const provider: LLMProvider = {
      async complete() {
        return { role: "assistant" as const, content: "unused", toolCalls: [] }
      },
      async *stream() {
        callCount += 1
        if (callCount === 1) {
          yield { type: "tool_call", id: "call_1", name: "fail_tool", arguments: {} } as StreamEvent
          return
        }
        yield { type: "text_delta", delta: "Recovered" } as StreamEvent
      }
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("fail_tool", "Fails always", { type: "object", properties: {} }, () => {
        throw new Error("Tool crashed!")
      })],
      { maxTurns: 4 }
    )

    const sessionId = "test-rollback"
    const text = await collectText(runner.run({ sessionId, goal: "run" }))
    expect(text).toBe("Recovered")

    const events = await sessionLog.read(sessionId)
    expect(events.some(e => e.event.kind === "rollbacked")).toBe(false)

    const { replayMessages } = await import("../../src/runtime/runner.js")
    const msgs = replayMessages(events)
    expect(msgs).toHaveLength(4)
    expect(msgs[0].role).toBe("user")
    expect(msgs[1].role).toBe("assistant")
    expect(msgs[1].toolCalls?.[0]?.name).toBe("fail_tool")
    expect(msgs[2].role).toBe("tool")
    expect(msgs[2].contentParts?.[0]).toEqual(expect.objectContaining({
      type: "tool_result",
      callId: "call_1",
      isError: true,
    }))
    expect(msgs[3].role).toBe("assistant")
    expect(msgs[3].content).toBe("Recovered")
  })
})
