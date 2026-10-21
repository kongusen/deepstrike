import {
  THINKING_INTENSITY_EXTENSION_KEY,
  projectAnthropicThinking,
  projectOpenAIResponsesThinking,
  projectDeepSeekThinking,
  projectQwenThinking,
} from "../src/providers/thinking-config.js"

describe("thinking-config projectors", () => {
  describe("projectAnthropicThinking", () => {
    it("returns undefined for auto (no override)", () => {
      expect(projectAnthropicThinking("auto")).toBeUndefined()
    })

    it("maps minimal to budget_tokens 1024 with interleaved-thinking beta", () => {
      const result = projectAnthropicThinking("minimal")!
      expect(result.params).toEqual({ thinking: { type: "enabled", budget_tokens: 1024 } })
      expect(result.betas).toContain("interleaved-thinking-2025-05-14")
    })

    it("maps standard to budget_tokens 8000", () => {
      const result = projectAnthropicThinking("standard")!
      expect(result.params.thinking).toEqual({ type: "enabled", budget_tokens: 8000 })
    })

    it("maps deep to budget_tokens 32000", () => {
      const result = projectAnthropicThinking("deep")!
      expect(result.params.thinking).toEqual({ type: "enabled", budget_tokens: 32000 })
    })
  })

  describe("projectOpenAIResponsesThinking", () => {
    it("returns undefined for auto", () => {
      expect(projectOpenAIResponsesThinking("auto")).toBeUndefined()
    })

    it("maps minimal to effort low", () => {
      expect(projectOpenAIResponsesThinking("minimal")).toEqual({ reasoning: { effort: "low" } })
    })

    it("maps standard to effort medium", () => {
      expect(projectOpenAIResponsesThinking("standard")).toEqual({ reasoning: { effort: "medium" } })
    })

    it("maps deep to effort high", () => {
      expect(projectOpenAIResponsesThinking("deep")).toEqual({ reasoning: { effort: "high" } })
    })
  })

  describe("projectDeepSeekThinking", () => {
    it("returns undefined for auto", () => {
      expect(projectDeepSeekThinking("auto")).toBeUndefined()
    })

    it("maps minimal to disabled thinking", () => {
      const result = projectDeepSeekThinking("minimal")!
      expect(result.__deepstrikeThinkingEnabled).toBe(false)
      expect(result.extra_body).toEqual({ thinking: { type: "disabled" } })
      expect(result.reasoning_effort).toBe("high")
    })

    it("maps standard to enabled thinking with effort high", () => {
      const result = projectDeepSeekThinking("standard")!
      expect(result.__deepstrikeThinkingEnabled).toBe(true)
      expect(result.extra_body).toEqual({ thinking: { type: "enabled" } })
      expect(result.reasoning_effort).toBe("high")
    })

    it("maps deep to enabled thinking with effort max", () => {
      const result = projectDeepSeekThinking("deep")!
      expect(result.reasoning_effort).toBe("max")
      expect(result.__deepstrikeThinkingEnabled).toBe(true)
    })
  })

  describe("projectQwenThinking", () => {
    it("returns undefined for auto", () => {
      expect(projectQwenThinking("auto")).toBeUndefined()
    })

    it("maps minimal to disabled thinking", () => {
      expect(projectQwenThinking("minimal")).toEqual({ extra_body: { enable_thinking: false } })
    })

    it("maps standard to budget 4096", () => {
      const result = projectQwenThinking("standard")!
      expect(result.extra_body).toMatchObject({ enable_thinking: true, thinking_budget: 4096 })
    })

    it("maps deep to budget 16384", () => {
      const result = projectQwenThinking("deep")!
      expect(result.extra_body).toMatchObject({ enable_thinking: true, thinking_budget: 16384 })
    })
  })

  describe("THINKING_INTENSITY_EXTENSION_KEY", () => {
    it("is a stable string key", () => {
      expect(typeof THINKING_INTENSITY_EXTENSION_KEY).toBe("string")
      expect(THINKING_INTENSITY_EXTENSION_KEY).toBe("thinkingIntensity")
    })
  })
})
