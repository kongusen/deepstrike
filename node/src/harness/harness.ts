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
  /** Feedback from the evaluator LLM — injected into the next attempt's goal. */
  feedback?: string
}

async function runOnce(agent: Agent, req: HarnessRequest): Promise<HarnessOutcome> {
  let text = ""
  let done: DoneEvent | undefined
  for await (const evt of agent.runStreaming(req.goal, req.criteria, req.extensions)) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
    else if (evt.type === "done") done = evt as DoneEvent
  }
  return {
    result: text,
    passed: false,
    iterations: done?.iterations ?? 0,
    totalTokens: done?.totalTokens ?? 0,
    status: done?.status ?? "error",
  }
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

/** Retry loop driven by a pluggable QualityGate (not LLM-as-judge). */
export class EvalLoopHarness {
  constructor(
    private agent: Agent,
    private gate: QualityGate,
    private maxAttempts = 3,
  ) {}

  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    let outcome: HarnessOutcome = { result: "", passed: false, iterations: 0, totalTokens: 0, status: "error" }
    for (let i = 0; i < this.maxAttempts; i++) {
      outcome = await runOnce(this.agent, request)
      if (await this.gate.evaluate(request, outcome)) {
        return { ...outcome, passed: true }
      }
    }
    return outcome
  }
}

export interface HarnessLoopOptions {
  maxAttempts?: number
  /** Directory to write distilled skills into. Requires the agent to have skillDir set. */
  skillDir?: string
}

/**
 * Eval loop with LLM-as-judge and feedback injection.
 *
 * Each failed attempt feeds the evaluator's feedback back into the next goal,
 * so the agent knows *why* it failed. On success, if the evaluator proposes a
 * skill candidate it is written to `skillDir` for future sessions to reuse.
 */
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

    let outcome: HarnessOutcome = { result: "", passed: false, iterations: 0, totalTokens: 0, status: "error" }
    let currentGoal = request.goal

    for (let attempt = 1; attempt <= this.maxAttempts; attempt++) {
      outcome = await runOnce(this.agent, { ...request, goal: currentGoal })

      // Phase 1: kernel builds eval prompt (positional args per kernel API)
      const evalAction = pipeline.feedOutcome(
        request.goal,
        request.criteria ?? [],
        outcome.result,
        attempt,
      )
      if (evalAction.kind !== "evaluate") break

      // Phase 2: SDK calls evaluator LLM
      let evalText = ""
      for await (const evt of this.evalProvider.stream(evalAction.messages ?? [], [], undefined)) {
        if (evt.type === "text_delta") evalText += (evt as TextDelta).delta
      }

      // Phase 3: kernel parses verdict
      const doneAction = pipeline.feedEvalResult(evalText)
      if (doneAction.kind !== "done") break

      outcome = { ...outcome, passed: doneAction.passed!, feedback: doneAction.feedback! }

      if (doneAction.passed) {
        if (doneAction.skill_candidate && this.skillDir) {
          const { name, description, whenToUse, content } = doneAction.skill_candidate
          const frontmatter = [
            "---",
            `name: ${name}`,
            `description: ${description}`,
            whenToUse ? `when_to_use: ${whenToUse}` : null,
            "---",
            "",
          ].filter(l => l !== null).join("\n")
          await writeFile(path.join(this.skillDir, `${name}.md`), frontmatter + content, "utf8")
        }
        return outcome
      }

      // Inject feedback into next attempt's goal
      currentGoal = `${request.goal}\n\n[Previous attempt ${attempt} failed: ${doneAction.feedback}]`
      pipeline.reset()
    }

    return outcome
  }
}
