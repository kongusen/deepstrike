import {
  buildLlmCompletedEvent,
  buildRunTerminalEvent,
  normalizeLlmCompleted,
  repairEventsForRecovery,
} from "../../src/runtime/session-repair.js"

describe("session-repair", () => {
  it("normalizes llm_completed with empty tool_calls", () => {
    const out = normalizeLlmCompleted({
      kind: "llm_completed",
      turn: 0,
      content: "hi",
    } as Parameters<typeof normalizeLlmCompleted>[0])
    expect(out.tool_calls).toEqual([])
    expect(out.token_count).toBeGreaterThan(0)
  })

  it("does NOT synthesize provider_replay during repair (provider-neutral)", () => {
    const repaired = repairEventsForRecovery([{
      seq: 0,
      event: {
        kind: "llm_completed",
        turn: 0,
        content: "checking",
        tool_calls: [{ id: "c1", name: "ping", arguments: "{}" }],
      },
    }])
    expect(repaired[0].event).toEqual(expect.objectContaining({
      kind: "llm_completed",
      tool_calls: [{ id: "c1", name: "ping", arguments: "{}" }],
    }))
    expect((repaired[0].event as { provider_replay?: unknown }).provider_replay).toBeUndefined()
  })

  it("passes a stored provider_replay envelope through verbatim", () => {
    const stored = { schema_version: 2 as const, provider: "deepseek", protocol: "openai-chat" as const, reasoning_content: "trace" }
    const repaired = repairEventsForRecovery([{
      seq: 0,
      event: {
        kind: "llm_completed",
        turn: 0,
        content: "x",
        tool_calls: [],
        provider_replay: stored,
      },
    }])
    expect((repaired[0].event as { provider_replay?: unknown }).provider_replay).toEqual(stored)
  })

  it("builds run_terminal with non-negative counters", () => {
    expect(buildRunTerminalEvent({ reason: "completed", turnsUsed: 1, totalTokens: 10 })).toEqual({
      kind: "run_terminal",
      reason: "completed",
      turns_used: 1,
      total_tokens: 10,
    })
  })

  it("buildLlmCompletedEvent always includes tool_calls array", () => {
    const event = buildLlmCompletedEvent({
      turn: 0,
      content: "done",
      toolCalls: [],
    })
    expect(event.tool_calls).toEqual([])
  })
})
