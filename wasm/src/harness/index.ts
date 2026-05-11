import type { Agent } from "../agent.js"
import type { DoneEvent, TextDelta } from "../types.js"

export interface HarnessRequest {
  goal: string
  criteria?: string[]
  extensions?: Record<string, unknown>
}

export interface HarnessOutcome {
  result: string
  passed: boolean
  iterations: number
  totalTokens: number
  status: string
  feedback?: string
}

async function runOnce(agent: Agent, goal: string, req: HarnessRequest): Promise<HarnessOutcome> {
  let text = ""
  let done: DoneEvent | undefined
  for await (const evt of agent.runStreaming(goal, req.criteria, req.extensions)) {
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

/**
 * Eval loop with LLM-as-judge and feedback injection.
 * Uses the kernel EvalPipeline — evaluator LLM assesses each attempt and
 * optionally proposes a skill candidate on success.
 */
export class HarnessLoop {
  private maxAttempts: number

  constructor(
    private agent: Agent,
    private evalProvider: import("../types.js").LLMProvider,
    options: HarnessLoopOptions = {},
  ) {
    this.maxAttempts = options.maxAttempts ?? 3
  }

  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    const kernel = await import("@deepstrike/wasm-kernel")
    const pipeline = new kernel.EvalPipeline({ extractSkillOnPass: true })

    let outcome: HarnessOutcome = { result: "", passed: false, iterations: 0, totalTokens: 0, status: "error" }
    let currentGoal = request.goal

    for (let attempt = 1; attempt <= this.maxAttempts; attempt++) {
      outcome = await runOnce(this.agent, currentGoal, request)

      const evalAction = pipeline.feedOutcome(request.goal, request.criteria ?? [], outcome.result, attempt)
      if (evalAction.kind !== "evaluate") break

      let evalText = ""
      for await (const evt of this.evalProvider.stream(evalAction.messages ?? [], [], undefined)) {
        if (evt.type === "text_delta") evalText += (evt as TextDelta).delta
      }

      const doneAction = pipeline.feedEvalResult({ content: evalText })
      if (doneAction.kind !== "done") break

      outcome = { ...outcome, passed: doneAction.passed ?? false, feedback: doneAction.feedback ?? undefined }

      if (outcome.passed) return outcome

      currentGoal = `${request.goal}\n\n[Previous attempt ${attempt} failed: ${doneAction.feedback}]`
      pipeline.reset()
    }

    return outcome
  }
}
