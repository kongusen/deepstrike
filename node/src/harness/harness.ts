import type { Agent } from "../agent.js"
import type { DoneEvent, TextDelta } from "../types.js"
import { writeFile } from "fs/promises"
import path from "path"

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function loadKernel(): Promise<any> {
  const mod = await import("@deepstrike/core")
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (mod as any).default ?? mod
}

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

async function runOnce(agent: Agent, req: HarnessRequest): Promise<HarnessOutcome> {
  let text = ""
  let done: DoneEvent | undefined
  for await (const evt of agent.runStreaming(req.goal, req.criteria?.map(c => c.text), req.extensions)) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
    else if (evt.type === "done") done = evt as DoneEvent
  }
  return { result: text, passed: false, iterations: done?.iterations ?? 0, totalTokens: done?.totalTokens ?? 0, status: done?.status ?? "error" }
}

export interface QualityGate {
  evaluate(request: HarnessRequest, outcome: HarnessOutcome): Promise<boolean>
}

export class SinglePassHarness {
  constructor(private agent: Agent) {}
  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    return { ...await runOnce(this.agent, request), passed: true }
  }
}

export class EvalLoopHarness {
  constructor(private agent: Agent, private gate: QualityGate, private maxAttempts = 3) {}

  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    let outcome: HarnessOutcome = { result: "", passed: false, iterations: 0, totalTokens: 0, status: "error" }
    for (let i = 0; i < this.maxAttempts; i++) {
      outcome = await runOnce(this.agent, request)
      if (await this.gate.evaluate(request, outcome)) return { ...outcome, passed: true }
    }
    return outcome
  }
}

export interface HarnessLoopOptions {
  maxAttempts?: number
  skillDir?: string
}

export class HarnessLoop {
  private maxAttempts: number
  private skillDir?: string

  constructor(
    private agent: Agent,
    private evalProvider: import("../types.js").LLMProvider,
    options: HarnessLoopOptions = {},
  ) {
    this.maxAttempts = options.maxAttempts ?? 3
    this.skillDir = options.skillDir
  }

  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    const kernel = await loadKernel()
    const pipeline = new kernel.EvalPipeline({ extractSkillOnPass: true })
    const criteria = request.criteria ?? []

    let outcome: HarnessOutcome = { result: "", passed: false, iterations: 0, totalTokens: 0, status: "error" }
    let currentGoal = request.goal

    for (let attempt = 1; attempt <= this.maxAttempts; attempt++) {
      outcome = await runOnce(this.agent, { ...request, goal: currentGoal })

      const evalAction = pipeline.feedOutcome(request.goal, criteria, outcome.result, attempt)
      if (evalAction.kind !== "evaluate") break

      let evalText = ""
      for await (const evt of this.evalProvider.stream(evalAction.messages ?? [], [], undefined)) {
        if (evt.type === "text_delta") evalText += (evt as TextDelta).delta
      }

      const doneAction = pipeline.feedEvalResult(evalText)
      if (doneAction.kind !== "done") break

      outcome = {
        ...outcome,
        passed: doneAction.passed ?? false,
        overallScore: doneAction.overallScore ?? undefined,
        feedback: doneAction.feedback ?? undefined,
        details: doneAction.details ?? undefined,
      }

      if (doneAction.passed) {
        if (doneAction.skill_candidate && this.skillDir) {
          const { name, description, whenToUse, content } = doneAction.skill_candidate
          const fm = ["---", `name: ${name}`, `description: ${description}`,
            whenToUse ? `when_to_use: ${whenToUse}` : null, "---", ""]
            .filter(Boolean).join("\n")
          await writeFile(path.join(this.skillDir, `${name}.md`), fm + content, "utf8")
        }
        return outcome
      }

      currentGoal = `${request.goal}\n\n[Previous attempt ${attempt} failed: ${doneAction.feedback}]`
      pipeline.reset()
    }

    return outcome
  }
}
