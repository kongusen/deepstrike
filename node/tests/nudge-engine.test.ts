/**
 * Self-Harness H1.2 — NudgeEngine.
 *  - trigger matrix ×5 (tool_error errorKind/toolName filters, tool_denied reasonIncludes,
 *    tool_calls_at_least cumulative first-reach, turns_at_least, entropy_alert).
 *  - template variable substitution ({{tool_name}} {{error_kind}} {{turn}}).
 *  - cooldownTurns / maxFires semantics (defaults 3 / 2).
 *  - engine purity: two engines with the same rules never share state.
 *  - load validation.
 */
import { NudgeEngine, validateNudgeRules, type NudgeRule } from "../src/harness/nudge.js"
import type { SessionEvent } from "../src/runtime/session-log.js"
import type { ToolErrorKind } from "../src/types.js"

const toolRequested = (turn: number, calls: Array<{ id: string; name: string }>): SessionEvent =>
  ({ kind: "tool_requested", turn, calls: calls.map(c => ({ id: c.id, name: c.name, arguments: "{}" })) })
const toolCompleted = (
  turn: number,
  results: Array<{ call_id: string; is_error?: boolean; error_kind?: string }>,
): SessionEvent =>
  ({ kind: "tool_completed", turn, results: results.map(r => ({ call_id: r.call_id, output: "out", is_error: r.is_error, error_kind: r.error_kind as ToolErrorKind | undefined })) })
const toolDenied = (turn: number, tool_name: string, reason: string): SessionEvent =>
  ({ kind: "tool_denied", turn, call_id: "cx", tool_name, reason })
const entropyAlert = (turn: number): SessionEvent =>
  ({ kind: "entropy_alert", turn, score: 0.9, threshold: 0.8 })
/** A turn-carrying event with no other side effects (advances currentTurn only). */
const turnMarker = (turn: number): SessionEvent => toolCompleted(turn, [])

describe("tool_error trigger", () => {
  it("fires on an is_error result with tool name and error kind in context", () => {
    const engine = new NudgeEngine([{ id: "e", on: { kind: "tool_error" }, note: "err {{tool_name}} kind={{error_kind}} @T{{turn}}" }])
    expect(engine.observe(toolRequested(1, [{ id: "c1", name: "writer" }]))).toEqual([])
    const out = engine.observe(toolCompleted(1, [{ call_id: "c1", is_error: true, error_kind: "invalid_arguments" }]))
    expect(out).toEqual([{ note: "err writer kind=invalid_arguments @T1", urgency: "normal" }])
  })

  it("does not fire on a clean result", () => {
    const engine = new NudgeEngine([{ id: "e", on: { kind: "tool_error" }, note: "x" }])
    expect(engine.observe(toolCompleted(1, [{ call_id: "c1", is_error: false }]))).toEqual([])
  })

  it("honors errorKind and toolName filters", () => {
    const engine = new NudgeEngine([
      { id: "only-timeout", on: { kind: "tool_error", errorKind: "timeout" }, note: "t" },
      { id: "only-writer", on: { kind: "tool_error", toolName: "writer" }, note: "w" },
    ])
    engine.observe(toolRequested(1, [{ id: "c1", name: "reader" }]))
    // reader + invalid_arguments matches neither the timeout nor the writer rule
    expect(engine.observe(toolCompleted(1, [{ call_id: "c1", is_error: true, error_kind: "invalid_arguments" }]))).toEqual([])
    engine.observe(toolRequested(4, [{ id: "c2", name: "writer" }]))
    const out = engine.observe(toolCompleted(4, [{ call_id: "c2", is_error: true, error_kind: "timeout" }]))
    expect(out.map(o => o.note).sort()).toEqual(["t", "w"]) // both rules match now
  })
})

describe("tool_denied trigger", () => {
  it("fires and filters on reasonIncludes substring", () => {
    const engine = new NudgeEngine([{ id: "d", on: { kind: "tool_denied", reasonIncludes: "repeat" }, note: "denied {{tool_name}}" }])
    expect(engine.observe(toolDenied(2, "writer", "governance: blocked"))).toEqual([])
    const out = engine.observe(toolDenied(3, "writer", "repeat-fuse: identical call x5"))
    expect(out).toEqual([{ note: "denied writer", urgency: "normal" }])
  })
})

describe("tool_calls_at_least trigger", () => {
  it("fires once when the cumulative count first reaches the threshold", () => {
    const engine = new NudgeEngine([{ id: "c3", on: { kind: "tool_calls_at_least", count: 3 }, note: "3 calls @T{{turn}}" }])
    expect(engine.observe(toolRequested(1, [{ id: "a", name: "t" }, { id: "b", name: "t" }]))).toEqual([]) // 2 < 3
    const out = engine.observe(toolRequested(2, [{ id: "c", name: "t" }, { id: "d", name: "t" }])) // 2 → 4 crosses 3
    expect(out).toEqual([{ note: "3 calls @T2", urgency: "normal" }])
    expect(engine.observe(toolRequested(3, [{ id: "e", name: "t" }]))).toEqual([]) // already crossed
  })
})

describe("turns_at_least trigger", () => {
  it("fires once when the turn first reaches the threshold", () => {
    const engine = new NudgeEngine([{ id: "t5", on: { kind: "turns_at_least", count: 5 }, note: "reached {{turn}}" }])
    expect(engine.observe(turnMarker(4))).toEqual([]) // 4 < 5
    expect(engine.observe(turnMarker(5))).toEqual([{ note: "reached 5", urgency: "normal" }])
    expect(engine.observe(turnMarker(6))).toEqual([]) // already crossed
  })
})

describe("entropy_alert trigger", () => {
  it("fires on the alert event and carries urgency", () => {
    const engine = new NudgeEngine([{ id: "ent", on: { kind: "entropy_alert" }, note: "entropy high @T{{turn}}", urgency: "high" }])
    expect(engine.observe(entropyAlert(7))).toEqual([{ note: "entropy high @T7", urgency: "high" }])
  })
})

describe("template substitution", () => {
  it("fills the three variables and leaves the ones with no value empty", () => {
    const engine = new NudgeEngine([{ id: "tpl", on: { kind: "turns_at_least", count: 1 }, note: "n={{tool_name}} k={{error_kind}} t={{turn}}" }])
    // a turns trigger has no tool/error context → those render empty
    expect(engine.observe(turnMarker(1))).toEqual([{ note: "n= k= t=1", urgency: "normal" }])
  })
})

describe("cooldownTurns / maxFires", () => {
  const errAt = (engine: NudgeEngine, turn: number) => {
    engine.observe(toolRequested(turn, [{ id: `c${turn}`, name: "t" }]))
    return engine.observe(toolCompleted(turn, [{ call_id: `c${turn}`, is_error: true }]))
  }

  it("applies the defaults: maxFires 2, cooldownTurns 3", () => {
    const engine = new NudgeEngine([{ id: "e", on: { kind: "tool_error" }, note: "err@{{turn}}" }])
    expect(errAt(engine, 1)).toHaveLength(1) // fire 1
    expect(errAt(engine, 2)).toHaveLength(0) // cooldown (2-1 < 3)
    expect(errAt(engine, 3)).toHaveLength(0) // cooldown (3-1 < 3)
    expect(errAt(engine, 4)).toHaveLength(1) // fire 2 (4-1 = 3)
    expect(errAt(engine, 8)).toHaveLength(0) // maxFires 2 exhausted
  })

  it("honors explicit cooldownTurns 0 and a raised maxFires", () => {
    const engine = new NudgeEngine([{ id: "e", on: { kind: "tool_error" }, note: "x", cooldownTurns: 0, maxFires: 5 }])
    expect(errAt(engine, 1)).toHaveLength(1)
    expect(errAt(engine, 1)).toHaveLength(1) // no cooldown → fires again same turn
    expect(errAt(engine, 1)).toHaveLength(1)
  })
})

describe("engine purity", () => {
  it("two engines built from the same rules do not share state", () => {
    const rules: NudgeRule[] = [{ id: "e", on: { kind: "tool_error" }, note: "x", maxFires: 1 }]
    const a = new NudgeEngine(rules)
    const b = new NudgeEngine(rules)
    a.observe(toolRequested(1, [{ id: "c1", name: "t" }]))
    expect(a.observe(toolCompleted(1, [{ call_id: "c1", is_error: true }]))).toHaveLength(1) // a exhausts maxFires
    b.observe(toolRequested(1, [{ id: "c1", name: "t" }]))
    expect(b.observe(toolCompleted(1, [{ call_id: "c1", is_error: true }]))).toHaveLength(1) // b still pristine
  })

  it("mutating the caller's rules array after construction does not affect the engine", () => {
    const rules: NudgeRule[] = [{ id: "e", on: { kind: "tool_error" }, note: "original" }]
    const engine = new NudgeEngine(rules)
    rules[0].note = "mutated"
    engine.observe(toolRequested(1, [{ id: "c1", name: "t" }]))
    expect(engine.observe(toolCompleted(1, [{ call_id: "c1", is_error: true }]))[0].note).toBe("original")
  })
})

describe("validateNudgeRules", () => {
  it("rejects more than 16 rules", () => {
    const rules: NudgeRule[] = Array.from({ length: 17 }, (_, i) => ({ id: `r${i}`, on: { kind: "entropy_alert" }, note: "x" }))
    expect(() => validateNudgeRules(rules)).toThrow(/16/)
  })

  it("rejects an empty note and a note over 2000 chars", () => {
    expect(() => validateNudgeRules([{ id: "r", on: { kind: "entropy_alert" }, note: "" }])).toThrow(/note/)
    expect(() => validateNudgeRules([{ id: "r", on: { kind: "entropy_alert" }, note: "x".repeat(2001) }])).toThrow(/2000/)
  })

  it("rejects an unsupported template variable", () => {
    expect(() => validateNudgeRules([{ id: "r", on: { kind: "entropy_alert" }, note: "hi {{goal}}" }])).toThrow(/template variable/)
  })

  it("rejects a duplicate id and a non-positive count", () => {
    expect(() => validateNudgeRules([
      { id: "dup", on: { kind: "entropy_alert" }, note: "a" },
      { id: "dup", on: { kind: "entropy_alert" }, note: "b" },
    ])).toThrow(/duplicate/)
    expect(() => validateNudgeRules([{ id: "r", on: { kind: "turns_at_least", count: 0 }, note: "a" }])).toThrow(/count/)
  })
})
