import { sanitizeReplayText, truncateBytesAtCharBoundary } from "../../src/runtime/replay-sanitize.js"

describe("replay-sanitize", () => {
  it("truncates CJK on char boundary", () => {
    expect(truncateBytesAtCharBoundary("你好世界", 5)).toBe("你")
  })

  it("leaves short text unchanged", () => {
    expect(sanitizeReplayText("短文本")).toBe("短文本")
  })

  it("appends marker when over cap", () => {
    const text = "你".repeat(20_000)
    const out = sanitizeReplayText(text, 100)
    expect(out.endsWith("… [replay truncated]")).toBe(true)
  })
})
