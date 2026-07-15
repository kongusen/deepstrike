import type { RuntimeRunner } from "../runtime/runner.js"
import { getKernel } from "../runtime/kernel.js"
import type { Criterion, Verdict } from "../runtime/eval.js"
import type {
  DoneEvent,
  LLMProvider,
  RenderedContext,
  TextDelta,
  WorkflowNodesSubmittedEvent,
} from "../types.js"
import type { WorkflowNodeSpec } from "../runtime/types/agent.js"

export type { Criterion, Verdict } from "../runtime/eval.js"

export interface AttemptRequest {
  sessionId?: string
  goal: string
  criteria?: Criterion[]
  extensions?: Record<string, unknown>
  inheritEvents?: Array<{ seq: number; event: import("../runtime/session-log.js").SessionEvent }>
}

export interface AttemptBodyContext extends AttemptRequest {
  sessionId: string
  attempt: number
  contextInput?: string
}

export type AttemptProgressEvent =
  | { type: "token"; text: string }
  | { type: "tool_call"; id: string; name: string }
  | { type: "tool_delta"; callId: string; delta?: string; chunk?: Record<string, unknown> }
  | { type: "tool_suspend"; callId: string; suspensionId: string; payload?: Record<string, unknown> }
  | { type: "tool_result"; callId: string; content: string; isError: boolean }
  | { type: "workflow_nodes_submitted"; nodes: WorkflowNodeSpec[] }
  | { type: "body_error"; message: string }

export interface AttemptBodyTerminal {
  type: "body_done"
  runStatus: string
  result: string
  turns: number
  totalTokens: number
  submittedNodes?: WorkflowNodeSpec[]
}

export type AttemptBodyEvent = AttemptProgressEvent | AttemptBodyTerminal

export interface AttemptBody {
  run(context: AttemptBodyContext): AsyncIterable<AttemptBodyEvent>
}

export class RuntimeAttemptBody implements AttemptBody {
  constructor(private readonly runner: RuntimeRunner) {}

  async *run(context: AttemptBodyContext): AsyncIterable<AttemptBodyEvent> {
    if (context.contextInput) this.runner.injectNote(context.contextInput)
    let result = ""
    let done: DoneEvent | undefined
    const submittedNodes: WorkflowNodeSpec[] = []
    for await (const event of this.runner.run({
      sessionId: context.sessionId,
      goal: context.goal,
      criteria: (context.criteria ?? []).map(criterion => criterion.text),
      extensions: context.extensions,
      ...(context.attempt === 1 && context.inheritEvents
        ? { inheritEvents: context.inheritEvents }
        : {}),
    })) {
      if (event.type === "text_delta") {
        const text = (event as TextDelta).delta
        result += text
        yield { type: "token", text }
      } else if (event.type === "tool_call") {
        const call = event as unknown as { id: string; name: string }
        yield { type: "tool_call", id: call.id, name: call.name }
      } else if (event.type === "tool_delta") {
        const delta = event as unknown as { callId: string; delta?: string; chunk?: Record<string, unknown> }
        yield { type: "tool_delta", callId: delta.callId, delta: delta.delta, chunk: delta.chunk }
      } else if (event.type === "tool_suspend") {
        const suspended = event as unknown as { callId: string; suspensionId: string; payload?: Record<string, unknown> }
        yield { type: "tool_suspend", callId: suspended.callId, suspensionId: suspended.suspensionId, payload: suspended.payload }
      } else if (event.type === "tool_result") {
        const tool = event as unknown as { callId: string; content: string; isError: boolean }
        yield { type: "tool_result", callId: tool.callId, content: tool.content, isError: tool.isError }
      } else if (event.type === "workflow_nodes_submitted") {
        const nodes = (event as WorkflowNodesSubmittedEvent).nodes
        submittedNodes.push(...nodes)
        yield { type: "workflow_nodes_submitted", nodes }
      } else if (event.type === "error") {
        yield { type: "body_error", message: String((event as { message?: string }).message ?? "run failed") }
      } else if (event.type === "done") {
        done = event as DoneEvent
      }
    }
    yield {
      type: "body_done",
      runStatus: done?.status ?? "error",
      result,
      turns: done?.iterations ?? 0,
      totalTokens: done?.totalTokens ?? 0,
      ...(submittedNodes.length ? { submittedNodes } : {}),
    }
  }
}

export interface JudgeContext {
  goal: string
  criteria: Criterion[]
  attempt: number
  result: string
}

export interface JudgeResult { verdict: Verdict }
export interface AttemptJudge {
  judge(context: JudgeContext): Promise<JudgeResult | undefined>
}

export type VerdictFn = (
  context: JudgeContext,
) => Verdict | undefined | Promise<Verdict | undefined>

export class VerdictFnJudge implements AttemptJudge {
  constructor(private readonly verdictFn: VerdictFn) {}
  async judge(context: JudgeContext): Promise<JudgeResult | undefined> {
    const verdict = await this.verdictFn(context)
    return verdict ? { verdict } : undefined
  }
}

export class LlmEvalJudge implements AttemptJudge {
  constructor(private readonly provider: LLMProvider) {}
  async judge(context: JudgeContext): Promise<JudgeResult> {
    const kernel = await getKernel()
    const messages = kernel.buildEvalMessages(
      context.goal,
      context.criteria,
      context.result,
      context.attempt,
      false,
    ) as import("../types.js").Message[]
    const rendered: RenderedContext = {
      systemText: messages.filter(message => message.role === "system").map(message => message.content).join("\n\n"),
      turns: messages.filter(message => message.role !== "system"),
    }
    let text = ""
    for await (const event of this.provider.stream(rendered, [], undefined)) {
      if (event.type === "text_delta") text += (event as TextDelta).delta
    }
    if (!text) throw new Error("attempt judge produced no text")
    const parsed = kernel.parseVerdict(text)
    return {
      verdict: {
        passed: parsed.passed,
        overallScore: parsed.overallScore,
        feedback: parsed.feedback,
        details: (parsed.details ?? []) as Verdict["details"],
      },
    }
  }
}

export class HybridJudge implements AttemptJudge {
  constructor(private readonly primary: AttemptJudge, private readonly fallback: AttemptJudge) {}
  async judge(context: JudgeContext): Promise<JudgeResult | undefined> {
    return await this.primary.judge(context) ?? this.fallback.judge(context)
  }
}

export interface PreparedAttempt { sessionId: string; goal: string; contextInput?: string }
export type CarryPolicy = (context: {
  rootSessionId: string
  goal: string
  attempt: number
  previousVerdict?: Verdict
}) => PreparedAttempt | Promise<PreparedAttempt>

export const continueSession: CarryPolicy = context => ({
  sessionId: context.rootSessionId,
  goal: context.goal,
  ...(context.previousVerdict?.feedback ? { contextInput: context.previousVerdict.feedback } : {}),
})

export const freshWithFeedback: CarryPolicy = context => ({
  sessionId: context.attempt === 1 ? context.rootSessionId : crypto.randomUUID(),
  goal: context.previousVerdict?.feedback
    ? `${context.goal}\n\n[Attempt ${context.attempt - 1} feedback: ${context.previousVerdict.feedback}]`
    : context.goal,
})

export function freshWithDigest(
  digest: (verdict: Verdict, attempt: number) => string | Promise<string>,
): CarryPolicy {
  return async context => ({
    sessionId: context.attempt === 1 ? context.rootSessionId : crypto.randomUUID(),
    goal: context.previousVerdict
      ? `${context.goal}\n\n[Prior attempt digest: ${await digest(context.previousVerdict, context.attempt - 1)}]`
      : context.goal,
  })
}

export interface StopPolicy {
  maxAttempts: number
  maxTotalTokens?: number
  stopOnFailedVerdict?: boolean
}
export type AttemptOutcomeKind = "passed" | "failed_judge" | "exhausted" | "run_error"
export interface AttemptOutcome {
  outcome: AttemptOutcomeKind
  runStatus: string
  verdict?: Verdict
  result: string
  attempts: number
  turns: number
  totalTokens: number
  submittedNodes?: WorkflowNodeSpec[]
}
export type AttemptLoopEvent =
  | AttemptProgressEvent
  | { type: "judging"; attempt: number }
  | { type: "retrying"; attempt: number; verdict: Verdict }
  | { type: "completed"; outcome: AttemptOutcome }

export interface AttemptLoopOptions {
  body: AttemptBody
  judge: AttemptJudge
  carry?: CarryPolicy
  stop: StopPolicy
  onPass?: (context: { outcome: AttemptOutcome; judgeResult: JudgeResult }) => void | Promise<void>
}

export class AttemptLoop {
  private readonly carry: CarryPolicy
  constructor(private readonly options: AttemptLoopOptions) {
    if (!Number.isInteger(options.stop.maxAttempts) || options.stop.maxAttempts < 1) {
      throw new Error("AttemptLoop stop.maxAttempts must be a positive integer")
    }
    if (options.stop.maxTotalTokens !== undefined && options.stop.maxTotalTokens < 0) {
      throw new Error("AttemptLoop stop.maxTotalTokens must be non-negative")
    }
    this.carry = options.carry ?? continueSession
  }

  async run(request: AttemptRequest): Promise<AttemptOutcome> {
    let outcome: AttemptOutcome | undefined
    for await (const event of this.stream(request)) {
      if (event.type === "completed") outcome = event.outcome
    }
    if (!outcome) throw new Error("AttemptLoop ended without an outcome")
    return outcome
  }

  async *stream(request: AttemptRequest): AsyncIterable<AttemptLoopEvent> {
    const rootSessionId = request.sessionId ?? crypto.randomUUID()
    const criteria = request.criteria ?? []
    const submittedNodes: WorkflowNodeSpec[] = []
    let totalTokens = 0
    let totalTurns = 0
    let previousVerdict: Verdict | undefined

    for (let attempt = 1; attempt <= this.options.stop.maxAttempts; attempt++) {
      const prepared = await this.carry({ rootSessionId, goal: request.goal, attempt, previousVerdict })
      let terminal: AttemptBodyTerminal | undefined
      for await (const event of this.options.body.run({
        ...request,
        sessionId: prepared.sessionId,
        goal: prepared.goal,
        criteria,
        attempt,
        ...(prepared.contextInput ? { contextInput: prepared.contextInput } : {}),
      })) {
        if (event.type === "body_done") {
          terminal = event
          if (event.submittedNodes) submittedNodes.push(...event.submittedNodes)
        } else yield event
      }
      if (!terminal) throw new Error("AttemptBody ended without body_done")
      totalTokens += terminal.totalTokens
      totalTurns += terminal.turns
      const base = {
        runStatus: terminal.runStatus,
        result: terminal.result,
        attempts: attempt,
        turns: totalTurns,
        totalTokens,
        ...(submittedNodes.length ? { submittedNodes } : {}),
      }
      if (isRunError(terminal.runStatus)) {
        yield { type: "completed", outcome: { outcome: "run_error", ...base } }
        return
      }
      yield { type: "judging", attempt }
      const judged = await this.options.judge.judge({ goal: request.goal, criteria, attempt, result: terminal.result })
      if (!judged) throw new Error("AttemptLoop judge produced no verdict")
      if (judged.verdict.passed) {
        const outcome: AttemptOutcome = { outcome: "passed", ...base, verdict: judged.verdict }
        await this.options.onPass?.({ outcome, judgeResult: judged })
        yield { type: "completed", outcome }
        return
      }
      previousVerdict = judged.verdict
      const tokenLimitReached = this.options.stop.maxTotalTokens !== undefined
        && totalTokens >= this.options.stop.maxTotalTokens
      if (this.options.stop.stopOnFailedVerdict || attempt === this.options.stop.maxAttempts || tokenLimitReached) {
        yield {
          type: "completed",
          outcome: {
            outcome: this.options.stop.stopOnFailedVerdict ? "failed_judge" : "exhausted",
            ...base,
            verdict: judged.verdict,
          },
        }
        return
      }
      yield { type: "retrying", attempt, verdict: judged.verdict }
    }
  }
}

function isRunError(status: string): boolean {
  return ["error", "invalid_arg", "user_abort"].includes(status.toLocaleLowerCase())
}
