import { buildEvalMessages, parseVerdict, verdictOutputSchema, judge } from "../src/runtime/eval.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

// ── helpers ─────────────────────────────────────────────────────────────────

function makeStubProvider(responseText: string): LLMProvider {
  return {
    async complete(): Promise<Message> {
      return { role: "assistant", content: responseText }
    },
    async *stream(): AsyncIterable<StreamEvent> {
      yield { type: "text_delta", delta: responseText } as { type: "text_delta"; delta: string }
    },
  }
}

// ── unit tests ──────────────────────────────────────────────────────────────

describe("buildEvalMessages", () => {
  it("renders a system + user message with goal/criteria/result", () => {
    const msgs = buildEvalMessages(
      "Count to 3",
      [{ text: "agent says 1, 2, 3", required: true }],
      "I counted: 1, 2, 3",
    )
    expect(msgs.length).toBe(2)
    expect(msgs[0].role).toBe("system")
    expect(msgs[0].content).toMatch(/impartial evaluator/i)
    expect(msgs[1].role).toBe("user")
    expect(msgs[1].content).toContain("Count to 3")
    expect(msgs[1].content).toContain("agent says 1, 2, 3")
    expect(msgs[1].content).toContain("I counted: 1, 2, 3")
  })

  it("defaults required=true when omitted", () => {
    const msgs = buildEvalMessages("g", [{ text: "c1" }], "r")
    expect(msgs[1].content).toContain("[required]")
  })

  it("marks optional criteria when required=false", () => {
    const msgs = buildEvalMessages("g", [{ text: "c1", required: false }], "r")
    expect(msgs[1].content).toContain("[optional]")
  })
})

describe("verdictOutputSchema", () => {
  it("returns a JSON-Schema-shaped object with required fields", () => {
    const schema = verdictOutputSchema()
    expect(schema.type).toBe("object")
    expect(schema.required).toEqual(expect.arrayContaining(["passed", "overall_score", "feedback"]))
  })
})

describe("parseVerdict", () => {
  it("parses a well-formed verdict JSON", () => {
    const json = JSON.stringify({
      passed: true,
      overall_score: 0.92,
      feedback: "Looks correct.",
      details: [{ criterion: "c1", passed: true, score: 1.0, feedback: "ok" }],
    })
    const v = parseVerdict(json)
    expect(v.passed).toBe(true)
    expect(v.overallScore).toBeCloseTo(0.92)
    expect(v.feedback).toBe("Looks correct.")
    expect(v.details).toHaveLength(1)
    expect(v.details[0].criterion).toBe("c1")
  })

  it("handles a verdict without details (kernel may fill in empty array)", () => {
    const json = JSON.stringify({ passed: false, overall_score: 0.3, feedback: "no" })
    const v = parseVerdict(json)
    expect(v.passed).toBe(false)
    expect(Array.isArray(v.details)).toBe(true)
  })
})

describe("judge", () => {
  it("calls provider.stream once and returns the parsed verdict", async () => {
    const verdictText = JSON.stringify({
      passed: true,
      overall_score: 0.85,
      feedback: "Agent counted correctly.",
      details: [{ criterion: "agent says 1, 2, 3", passed: true, score: 1.0, feedback: "match" }],
    })
    const provider = makeStubProvider(verdictText)
    const verdict = await judge({
      provider,
      goal: "Count to 3",
      criteria: [{ text: "agent says 1, 2, 3" }],
      result: "I counted: 1, 2, 3",
    })
    expect(verdict.passed).toBe(true)
    expect(verdict.overallScore).toBeCloseTo(0.85)
    expect(verdict.feedback).toContain("counted")
  })

  it("throws when provider produces no text", async () => {
    const emptyProvider: LLMProvider = {
      async complete(): Promise<Message> { return { role: "assistant", content: "" } },
      // eslint-disable-next-line require-yield
      async *stream(): AsyncIterable<StreamEvent> { return },
    }
    await expect(
      judge({ provider: emptyProvider, goal: "g", criteria: [{ text: "c" }], result: "r" }),
    ).rejects.toThrow(/no text/)
  })

  it("renders system + user messages in RenderedContext correctly", async () => {
    let capturedCtx: RenderedContext | undefined
    const captureProvider: LLMProvider = {
      async complete(): Promise<Message> { return { role: "assistant", content: "{}" } },
      async *stream(ctx: RenderedContext, _tools: ToolSchema[]): AsyncIterable<StreamEvent> {
        capturedCtx = ctx
        yield { type: "text_delta", delta: JSON.stringify({ passed: true, overall_score: 1, feedback: "ok" }) } as { type: "text_delta"; delta: string }
      },
    }
    await judge({ provider: captureProvider, goal: "g", criteria: [{ text: "c" }], result: "r" })
    expect(capturedCtx).toBeDefined()
    expect(capturedCtx!.systemText).toMatch(/impartial evaluator/i)
    expect(capturedCtx!.turns).toHaveLength(1)
    expect(capturedCtx!.turns[0].role).toBe("user")
    expect(capturedCtx!.turns[0].content).toContain("g")
    expect(capturedCtx!.turns[0].content).toContain("c")
    expect(capturedCtx!.turns[0].content).toContain("r")
  })
})
