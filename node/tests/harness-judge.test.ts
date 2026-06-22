import { describe, it, expect } from "@jest/globals"
import { VerdictFnJudge, LlmEvalJudge, HybridJudge, type AttemptJudge, type JudgeContext, type JudgeResult } from "../src/harness/judge.js"
import type { VerdictFn } from "../src/harness/harness.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema, ProviderRunState } from "../src/types.js"

const CTX: JudgeContext = { goal: "g", criteria: [{ text: "c1", required: true }], attempt: 1, result: "out" }

function evalProvider(verdictJson: string): { provider: LLMProvider; calls: () => number } {
  const state = { n: 0 }
  const provider: LLMProvider = {
    async complete(): Promise<Message> { return { role: "assistant", content: verdictJson } },
    async *stream(_c: RenderedContext, _t: ToolSchema[], _e?: Record<string, unknown>, _s?: ProviderRunState): AsyncIterable<StreamEvent> {
      state.n++
      yield { type: "text_delta", delta: verdictJson } as StreamEvent
      yield { type: "done", iterations: 1, totalTokens: 10, status: "completed" } as StreamEvent
    },
  }
  return { provider, calls: () => state.n }
}

const PASS_JSON = JSON.stringify({ passed: true, overall_score: 1, feedback: "ok", details: [] })

describe("AttemptJudge strategies (P4)", () => {
  it("VerdictFnJudge returns {verdict} when the fn returns a verdict", async () => {
    const fn: VerdictFn = () => ({ passed: false, overallScore: 0, feedback: "nope", details: [] })
    const res = await new VerdictFnJudge(fn).judge(CTX)
    expect(res?.verdict.passed).toBe(false)
    expect(res?.verdict.feedback).toBe("nope")
  })

  it("VerdictFnJudge defers (undefined) when the fn returns undefined", async () => {
    const fn: VerdictFn = () => undefined
    expect(await new VerdictFnJudge(fn).judge(CTX)).toBeUndefined()
  })

  it("LlmEvalJudge streams the eval provider and parses the verdict", async () => {
    const { provider, calls } = evalProvider(PASS_JSON)
    const res = await new LlmEvalJudge(provider).judge(CTX)
    expect(calls()).toBe(1)
    expect(res.verdict.passed).toBe(true)
  })

  it("HybridJudge uses the primary and skips the fallback when the primary returns", async () => {
    const { provider, calls } = evalProvider(PASS_JSON)
    const primary: AttemptJudge = { async judge(): Promise<JudgeResult> { return { verdict: { passed: true, overallScore: 1, feedback: "host", details: [] } } } }
    const res = await new HybridJudge(primary, new LlmEvalJudge(provider)).judge(CTX)
    expect(res?.verdict.feedback).toBe("host")
    expect(calls()).toBe(0) // fallback never invoked
  })

  it("HybridJudge falls back when the primary defers", async () => {
    const { provider, calls } = evalProvider(PASS_JSON)
    const primary: AttemptJudge = { async judge(): Promise<JudgeResult | undefined> { return undefined } }
    const res = await new HybridJudge(primary, new LlmEvalJudge(provider)).judge(CTX)
    expect(calls()).toBe(1)
    expect(res?.verdict.passed).toBe(true)
  })
})
