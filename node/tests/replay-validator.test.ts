import {
  validateOpenAIChatReplay,
  assessToolResultPairing,
  patchMissingToolResults,
  DEGRADED_TOOL_RESULT_OUTPUT,
  ProviderReplayValidationError,
} from "../src/providers/replay-validator.js"
import type { ModelMessage, RenderedContext } from "../src/types.js"

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function ctx(turns: ModelMessage[]): RenderedContext {
  return { systemText: "", turns: [...turns] }
}

const user = (content = "go"): ModelMessage => ({ role: "user", content, toolCalls: [] })
const asst = (content: string, callIds: string[] = []): ModelMessage => ({
  role: "assistant",
  content,
  toolCalls: callIds.map(id => ({ id, name: "tool_fn", arguments: "{}" })),
})
const tool = (callId: string, isError = false): ModelMessage => ({
  role: "tool",
  content: "",
  toolCalls: [],
  contentParts: [{ type: "tool_result", callId, output: "ok", isError }],
})

// ---------------------------------------------------------------------------
// assessToolResultPairing
// ---------------------------------------------------------------------------

describe("assessToolResultPairing", () => {
  it("returns ok when every tool call has a result", () => {
    const turns: ModelMessage[] = [user(), asst("", ["c1"]), tool("c1"), asst("done")]
    expect(assessToolResultPairing(turns)).toEqual({ ok: true, missingCallIds: [] })
  })

  it("reports a missing result at the end of history (interrupted tail)", () => {
    const turns: ModelMessage[] = [user(), asst("", ["c1"])]
    const result = assessToolResultPairing(turns)
    expect(result.ok).toBe(false)
    expect(result.missingCallIds).toEqual(["c1"])
  })

  it("reports a missing result in the middle of history", () => {
    // assistant called c1 but no tool result — user message follows
    const turns: ModelMessage[] = [user(), asst("", ["c1"]), user("continue")]
    const result = assessToolResultPairing(turns)
    expect(result.ok).toBe(false)
    expect(result.missingCallIds).toContain("c1")
  })

  it("reports only the missing call ids when some are answered", () => {
    const turns: ModelMessage[] = [
      user(),
      asst("", ["c1", "c2"]),
      tool("c1"), // c2 has no result
    ]
    const result = assessToolResultPairing(turns)
    expect(result.ok).toBe(false)
    expect(result.missingCallIds).toEqual(["c2"])
  })

  it("returns ok for a history with no tool calls", () => {
    const turns: ModelMessage[] = [user(), asst("plain response")]
    expect(assessToolResultPairing(turns)).toEqual({ ok: true, missingCallIds: [] })
  })
})

// ---------------------------------------------------------------------------
// patchMissingToolResults
// ---------------------------------------------------------------------------

describe("patchMissingToolResults", () => {
  it("returns the same reference when nothing needs patching", () => {
    const turns: ModelMessage[] = [user(), asst("", ["c1"]), tool("c1"), asst("done")]
    expect(patchMissingToolResults(turns)).toBe(turns)
  })

  it("injects a synthetic error result for an interrupted tail", () => {
    const turns: ModelMessage[] = [user(), asst("", ["c1"])]
    const out = patchMissingToolResults(turns)
    expect(out).not.toBe(turns)
    expect(out).toHaveLength(3)
    expect(out[2]).toMatchObject({
      role: "tool",
      contentParts: [{ type: "tool_result", callId: "c1", isError: true, output: DEGRADED_TOOL_RESULT_OUTPUT }],
    })
  })

  it("injects a synthetic result mid-history and preserves subsequent messages", () => {
    const turns: ModelMessage[] = [
      user(),
      asst("", ["c1"]),
      // no tool result — interrupted
      asst("recovered"), // next assistant turn
    ]
    const out = patchMissingToolResults(turns)
    expect(out).toHaveLength(4)
    expect(out[2]).toMatchObject({ role: "tool" })
    expect(out[3]).toMatchObject({ role: "assistant", content: "recovered" })
  })

  it("preserves existing tool results and only patches missing ones", () => {
    // c1 answered, c2 missing
    const turns: ModelMessage[] = [user(), asst("", ["c1", "c2"]), tool("c1")]
    const out = patchMissingToolResults(turns)
    expect(out).toHaveLength(4) // original 3 + 1 synthetic
    expect(out[2]).toMatchObject({ role: "tool" })
    const cp2 = out[3]?.contentParts?.[0]
    expect(cp2).toMatchObject({ type: "tool_result", callId: "c2", isError: true })
  })

  it("handles multiple interrupted turns in a single history", () => {
    const turns: ModelMessage[] = [
      user(),
      asst("", ["c1"]),
      // c1 missing
      asst("", ["c2"]),
      tool("c2"),
      asst("done"),
    ]
    const out = patchMissingToolResults(turns)
    // Should inject exactly one synthetic result for c1
    const synth = out.filter(m => m.contentParts?.some(p => p.type === "tool_result" && p.isError && (p as { output?: string }).output === DEGRADED_TOOL_RESULT_OUTPUT))
    expect(synth).toHaveLength(1)
    expect(synth[0]!.contentParts![0]).toMatchObject({ callId: "c1" })
  })
})

// ---------------------------------------------------------------------------
// validateOpenAIChatReplay — strict mode (default)
// ---------------------------------------------------------------------------

describe("validateOpenAIChatReplay strict mode", () => {
  it("does not throw for a well-formed history", () => {
    const c = ctx([user(), asst("", ["c1"]), tool("c1"), asst("done")])
    expect(() => validateOpenAIChatReplay(c)).not.toThrow()
  })

  it("throws ProviderReplayValidationError for a missing tool result", () => {
    const c = ctx([user(), asst("", ["c1"])])
    expect(() => validateOpenAIChatReplay(c)).toThrow(ProviderReplayValidationError)
  })

  it("throws for an orphan tool result with no preceding tool_call", () => {
    const c = ctx([user(), tool("c1")])
    expect(() => validateOpenAIChatReplay(c)).toThrow(ProviderReplayValidationError)
  })

  it("throws for a duplicate tool result", () => {
    const c = ctx([user(), asst("", ["c1"]), tool("c1"), tool("c1")])
    expect(() => validateOpenAIChatReplay(c)).toThrow(ProviderReplayValidationError)
  })
})

// ---------------------------------------------------------------------------
// validateOpenAIChatReplay — degradeMissingToolResult mode
// ---------------------------------------------------------------------------

describe("validateOpenAIChatReplay degradeMissingToolResult", () => {
  it("does not throw for an interrupted tail when degradeMissingToolResult is true", () => {
    const c = ctx([user(), asst("", ["c1"])])
    expect(() => validateOpenAIChatReplay(c, { degradeMissingToolResult: true })).not.toThrow()
  })

  it("patches context.turns in place with synthetic tool results", () => {
    const c = ctx([user(), asst("", ["c1"])])
    validateOpenAIChatReplay(c, { degradeMissingToolResult: true })
    expect(c.turns).toHaveLength(3)
    expect(c.turns[2]).toMatchObject({
      role: "tool",
      contentParts: [{ type: "tool_result", callId: "c1", isError: true }],
    })
  })

  it("leaves context.turns unchanged when no patching is needed", () => {
    const turns: ModelMessage[] = [user(), asst("", ["c1"]), tool("c1"), asst("done")]
    const c = ctx(turns)
    const originalTurnsRef = c.turns
    validateOpenAIChatReplay(c, { degradeMissingToolResult: true })
    // No patching needed: array identity and contents must be stable
    expect(c.turns).toHaveLength(4)
    // In-place splice would have replaced the array contents but the reference
    // stays the same (splice mutates, it doesn't replace the variable).
    expect(c.turns).toBe(originalTurnsRef)
  })

  it("still validates strict pairing when degradeMissingToolResult is false (default)", () => {
    const c = ctx([user(), asst("", ["c1"])])
    expect(() => validateOpenAIChatReplay(c, { degradeMissingToolResult: false })).toThrow(ProviderReplayValidationError)
  })

  it("still throws for a duplicate tool result even in degrade mode", () => {
    // duplicate results are a data corruption issue — degrade mode does not suppress them
    const c = ctx([user(), asst("", ["c1"]), tool("c1"), tool("c1")])
    expect(() => validateOpenAIChatReplay(c, { degradeMissingToolResult: true })).toThrow(ProviderReplayValidationError)
  })
})
