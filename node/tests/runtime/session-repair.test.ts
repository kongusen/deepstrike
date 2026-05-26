import {
  buildLlmCompletedEvent,
  buildRunTerminalEvent,
  effectiveProviderReplay,
  normalizeLlmCompleted,
  repairEventsForRecovery,
  synthesizeProviderReplay,
} from "../../src/runtime/session-repair.js"

describe("session-repair", () => {
  it("synthesizes provider_replay for tool turns", () => {
    const replay = synthesizeProviderReplay("checking", [
      { id: "c1", name: "ping", arguments: "{}" },
    ])
    expect(replay?.native_blocks).toHaveLength(2)
  })

  it("normalizes llm_completed with empty tool_calls", () => {
    const out = normalizeLlmCompleted({
      kind: "llm_completed",
      turn: 0,
      content: "hi",
    } as Parameters<typeof normalizeLlmCompleted>[0])
    expect(out.tool_calls).toEqual([])
    expect(out.token_count).toBeGreaterThan(0)
  })

  it("fills missing provider_replay during repair", () => {
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
      provider_replay: expect.objectContaining({ native_blocks: expect.any(Array) }),
    }))
  })

  it("prefers stored reasoning replay", () => {
    const replay = effectiveProviderReplay("x", [], { reasoning_content: "trace" })
    expect(replay?.reasoning_content).toBe("trace")
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
