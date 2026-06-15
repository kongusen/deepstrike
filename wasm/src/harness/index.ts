import type { RuntimeRunner } from "../runtime/runner.js"
import { collectText } from "../runtime/runner.js"
import type { DoneEvent, TextDelta } from "../types.js"

export interface Criterion {
  text: string
  required: boolean
  weight?: number
}

export interface CriterionResult {
  criterion: string
  passed: boolean
  score: number
  feedback: string
}

export interface HarnessRequest {
  goal: string
  criteria?: Criterion[]
  extensions?: Record<string, unknown>
}

export interface HarnessOutcome {
  result: string
  passed: boolean
  iterations: number
  totalTokens: number
  status: string
  overallScore?: number
  feedback?: string
  details?: CriterionResult[]
}

export interface Verdict {
  passed: boolean
  overallScore: number
  feedback: string
  details: CriterionResult[]
}

export type HarnessEvent =
  | { type: "token"; text: string }
  | { type: "tool_call"; id: string; name: string }
  | { type: "tool_result"; callId: string; content: string; isError: boolean }
  | { type: "supervising" }
  | { type: "revising"; verdict: Verdict }
  | { type: "done"; verdict: Verdict; iterations: number; totalTokens: number; status: string }
  | { type: "max_attempts_reached" }

async function runOnce(runner: RuntimeRunner, req: HarnessRequest): Promise<HarnessOutcome> {
  let text = ""
  let done: DoneEvent | undefined
  const sessionId = crypto.randomUUID()
  for await (const evt of runner.run({ sessionId, goal: req.goal, criteria: req.criteria?.map(c => c.text), extensions: req.extensions })) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
    else if (evt.type === "done") done = evt as DoneEvent
  }
  return { result: text, passed: false, iterations: done?.iterations ?? 0, totalTokens: done?.totalTokens ?? 0, status: done?.status ?? "error" }
}

export class SinglePassHarness {
  constructor(private runner: RuntimeRunner) {}
  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    return { ...await runOnce(this.runner, request), passed: true }
  }
}

export interface HarnessLoopOptions {
  maxAttempts?: number
}

export class HarnessLoop {
  private maxAttempts: number

  constructor(
    private runner: RuntimeRunner,
    private evalProvider: import("../types.js").LLMProvider,
    options: HarnessLoopOptions = {},
  ) {
    this.maxAttempts = options.maxAttempts ?? 3
  }

  async *runStreaming(request: HarnessRequest): AsyncIterable<HarnessEvent> {
    const kernel = await import("@deepstrike/wasm-kernel")
    const criteria = request.criteria ?? []

    let currentGoal = request.goal
    let lastIterations = 0
    let lastTotalTokens = 0
    let lastStatus = "error"
    let lastResult = ""
    const sessionId = crypto.randomUUID()

    for (let attempt = 1; attempt <= this.maxAttempts; attempt++) {
      for await (const evt of this.runner.run({ sessionId, goal: currentGoal, criteria: criteria.map(c => c.text), extensions: request.extensions })) {
        if (evt.type === "text_delta") {
          lastResult += (evt as TextDelta).delta
          yield { type: "token", text: (evt as TextDelta).delta }
        } else if (evt.type === "tool_call") {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const tc = evt as any
          yield { type: "tool_call", id: tc.id, name: tc.name }
        } else if (evt.type === "tool_result") {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const tr = evt as any
          yield { type: "tool_result", callId: tr.callId, content: tr.content, isError: tr.isError }
        } else if (evt.type === "done") {
          const d = evt as DoneEvent
          lastIterations = d.iterations
          lastTotalTokens = d.totalTokens
          lastStatus = d.status
        }
      }

      yield { type: "supervising" }

      // #6 (0.5.0): eval/verdict compute is the kernel's stateless free functions (was EvalPipeline).
      const evalMsgs = kernel.buildEvalMessages(request.goal, criteria, lastResult, attempt, true)
      let evalText = ""
      const evalContext: import("../types.js").RenderedContext = {
        systemText: "",
        turns: evalMsgs as import("../types.js").Message[],
      }
      for await (const evt of this.evalProvider.stream(evalContext, [], undefined)) {
        if (evt.type === "text_delta") evalText += (evt as TextDelta).delta
      }

      const parsed = kernel.parseVerdict(evalText)
      const verdict: Verdict = {
        passed: parsed.passed,
        overallScore: parsed.overallScore,
        feedback: parsed.feedback,
        details: (parsed.details ?? []) as CriterionResult[],
      }

      if (verdict.passed) {
        yield { type: "done", verdict, iterations: lastIterations, totalTokens: lastTotalTokens, status: lastStatus }
        return
      }

      yield { type: "revising", verdict }
      currentGoal = `${request.goal}\n\n[Attempt ${attempt} feedback: ${verdict.feedback}]`
      lastResult = ""
    }

    yield { type: "max_attempts_reached" }
  }
}
