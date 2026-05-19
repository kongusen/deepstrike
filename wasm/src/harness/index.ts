import type { Agent } from "../agent.js"
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

async function runOnce(agent: Agent, goal: string, req: HarnessRequest): Promise<HarnessOutcome> {
  let text = ""
  let done: DoneEvent | undefined
  for await (const evt of agent.runStreaming(goal, (req.criteria ?? []).map(c => c.text), req.extensions)) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
    else if (evt.type === "done") done = evt as DoneEvent
  }
  return { result: text, passed: false, iterations: done?.iterations ?? 0, totalTokens: done?.totalTokens ?? 0, status: done?.status ?? "error" }
}

export class SinglePassHarness {
  constructor(private agent: Agent) {}
  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    return { ...await runOnce(this.agent, request.goal, request), passed: true }
  }
}

export interface HarnessLoopOptions {
  maxAttempts?: number
}

export class HarnessLoop {
  private maxAttempts: number

  constructor(
    private agent: Agent,
    private evalProvider: import("../types.js").LLMProvider,
    options: HarnessLoopOptions = {},
  ) {
    this.maxAttempts = options.maxAttempts ?? 3
  }

  async *runStreaming(request: HarnessRequest): AsyncIterable<HarnessEvent> {
    const kernel = await import("@deepstrike/wasm-kernel")
    const pipeline = new kernel.EvalPipeline({ extractSkillOnPass: true })
    const criteria = request.criteria ?? []

    let currentGoal = request.goal
    let lastIterations = 0
    let lastTotalTokens = 0
    let lastStatus = "error"
    let lastResult = ""

    for (let attempt = 1; attempt <= this.maxAttempts; attempt++) {
      for await (const evt of this.agent.runStreaming(currentGoal, criteria.map(c => c.text), request.extensions)) {
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

      const evalAction = pipeline.feedOutcome(request.goal, criteria, lastResult, attempt)
      if (evalAction.kind !== "evaluate") break

      let evalText = ""
      const evalContext: import("../types.js").RenderedContext = {
        systemText: "",
        turns: (evalAction.messages ?? []) as import("../types.js").Message[],
      }
      for await (const evt of this.evalProvider.stream(evalContext, [], undefined)) {
        if (evt.type === "text_delta") evalText += (evt as TextDelta).delta
      }

      const doneAction = pipeline.feedEvalResult(evalText)
      if (doneAction.kind !== "done") break

      const verdict: Verdict = {
        passed: doneAction.passed ?? false,
        overallScore: doneAction.overallScore ?? 0,
        feedback: doneAction.feedback ?? "",
        details: doneAction.details ?? [],
      }

      if (verdict.passed) {
        yield { type: "done", verdict, iterations: lastIterations, totalTokens: lastTotalTokens, status: lastStatus }
        return
      }

      yield { type: "revising", verdict }
      currentGoal = `${request.goal}\n\n[Attempt ${attempt} feedback: ${verdict.feedback}]`
      lastResult = ""
      pipeline.reset()
    }

    yield { type: "max_attempts_reached" }
  }
}
