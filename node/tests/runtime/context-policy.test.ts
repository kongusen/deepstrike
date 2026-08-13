import {
  contextPolicy,
  normalizeContextPolicy,
  ratioToPpm,
} from "../../src/runtime/context-policy.js"

describe("ContextPolicy", () => {
  it("normalizes ergonomic ratios to the integer-only canonical wire", () => {
    const wire = normalizeContextPolicy(contextPolicy())
    expect(wire).toEqual({
      pressure_thresholds_ppm: {
        snip: 700_000,
        micro: 800_000,
        collapse: 900_000,
        auto: 950_000,
        renewal: 980_000,
      },
      target_after_compress_ppm: 650_000,
      preserve_recent_turns: 2,
      renewal_carryover_ppm: 50_000,
      collapse_old_assistant_narration: true,
      idle_micro_compact_minutes: 60,
    })
    expect(ratioToPpm(0.1234565)).toBe(123_457)
  })

  it("merges partial thresholds without exposing algorithm-only knobs", () => {
    const policy = contextPolicy({
      pressureThresholds: { snip: 0.72 },
      preserveRecentTurns: 4,
    })
    expect(policy.pressureThresholds).toEqual({
      snip: 0.72,
      micro: 0.80,
      collapse: 0.90,
      auto: 0.95,
      renewal: 0.98,
    })
    expect(policy.preserveRecentTurns).toBe(4)
  })

  it("rejects the whole policy when thresholds or bounds are invalid", () => {
    expect(() => contextPolicy({ pressureThresholds: { micro: 0.69 } })).toThrow("snip < micro")
    expect(() => contextPolicy({ targetAfterCompress: 0.70 })).toThrow("lower than the snip")
    expect(() => contextPolicy({ renewalCarryover: 1.1 })).toThrow("between 0 and 1")
    expect(() => contextPolicy({ preserveRecentTurns: 0 })).toThrow("safe integer >= 1")
  })
})
