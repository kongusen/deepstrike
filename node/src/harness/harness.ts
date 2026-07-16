import type { RuntimeRunner } from "../runtime/runner.js"
import type { SessionEvent } from "../runtime/session-log.js"
import type {
  ContentPart,
  DoneEvent,
  TextDelta,
  WorkflowNodesSubmittedEvent,
} from "../types.js"
import type { WorkflowNodeSpec } from "../types/agent.js"
import type { Criterion, Verdict } from "../runtime/eval.js"
import type { AttemptJudge, JudgeResult } from "./judge.js"

export type { Criterion, Verdict } from "../runtime/eval.js"

export interface AttemptRequest {
  sessionId?: string
  goal: string
  criteria?: Criterion[]
  /**
   * Multimodal inputs (images / audio) attached to the task. Forwarded to every attempt
   * unconditionally; the runner seeds them per session idempotently, so fresh-session carries
   * re-seed while same-session carries do not double.
   */
  attachments?: ContentPart[]
  extensions?: Record<string, unknown>
  /** Parent transcript inherited by the first attempt only. */
  inheritEvents?: Array<{ seq: number; event: SessionEvent }>
}

export interface AttemptBodyContext extends AttemptRequest {
  sessionId: string
  attempt: number
  /** Carry material delivered as context, never folded into `goal` by the default policy. */
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

/** Adapts RuntimeRunner to the body slot without giving the loop knowledge of kernel events. */
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
      ...(context.attachments?.length ? { attachments: context.attachments } : {}),
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
        yield {
          type: "tool_delta",
          callId: delta.callId,
          ...(delta.delta !== undefined ? { delta: delta.delta } : {}),
          ...(delta.chunk !== undefined ? { chunk: delta.chunk } : {}),
        }
      } else if (event.type === "tool_suspend") {
        const suspended = event as unknown as { callId: string; suspensionId: string; payload?: Record<string, unknown> }
        yield {
          type: "tool_suspend",
          callId: suspended.callId,
          suspensionId: suspended.suspensionId,
          ...(suspended.payload !== undefined ? { payload: suspended.payload } : {}),
        }
      } else if (event.type === "tool_result") {
        const toolResult = event as unknown as { callId: string; content: string; isError: boolean }
        yield {
          type: "tool_result",
          callId: toolResult.callId,
          content: toolResult.content,
          isError: toolResult.isError,
        }
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
      ...(submittedNodes.length > 0 ? { submittedNodes } : {}),
    }
  }
}

export type VerdictFn = (context: {
  goal: string
  criteria: Criterion[]
  attempt: number
  result: string
}) => Verdict | undefined | Promise<Verdict | undefined>

export interface PreparedAttempt {
  sessionId: string
  goal: string
  contextInput?: string
}

export type CarryPolicy = (context: {
  rootSessionId: string
  goal: string
  attempt: number
  previousVerdict?: Verdict
}) => PreparedAttempt | Promise<PreparedAttempt>

/** Default: retain the transcript and deliver judge feedback through the runner's signal input. */
export const continueSession: CarryPolicy = context => ({
  sessionId: context.rootSessionId,
  goal: context.goal,
  ...(context.previousVerdict?.feedback
    ? { contextInput: context.previousVerdict.feedback }
    : {}),
})

/** Explicit isolation policy preserving the old fresh-session + goal-feedback behavior. */
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
  /** Useful for one-shot gates; retry loops normally leave this false. */
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
  onPass?: (context: { outcome: AttemptOutcome; judgeResult: JudgeResult }) => Promise<void> | void
}

function isRunError(status: string): boolean {
  const normalized = status.toLocaleLowerCase()
  return normalized === "error" || normalized === "invalid_arg" || normalized === "user_abort"
}

/** One attempt engine. Body, judgment, carry, and stopping are independent policy slots. */
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
      const prepared = await this.carry({
        rootSessionId,
        goal: request.goal,
        attempt,
        previousVerdict,
      })
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
        } else {
          yield event
        }
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
        ...(submittedNodes.length > 0 ? { submittedNodes } : {}),
      }

      if (isRunError(terminal.runStatus)) {
        yield { type: "completed", outcome: { outcome: "run_error", ...base } }
        return
      }

      yield { type: "judging", attempt }
      const judged = await this.options.judge.judge({
        goal: request.goal,
        criteria,
        attempt,
        result: terminal.result,
      })
      if (!judged) throw new Error("AttemptLoop judge produced no verdict")
      const verdict = judged.verdict

      if (verdict.passed) {
        const outcome: AttemptOutcome = { outcome: "passed", ...base, verdict }
        await this.options.onPass?.({ outcome, judgeResult: judged })
        yield { type: "completed", outcome }
        return
      }

      previousVerdict = verdict
      const tokenLimitReached = this.options.stop.maxTotalTokens !== undefined
        && totalTokens >= this.options.stop.maxTotalTokens
      if (this.options.stop.stopOnFailedVerdict || attempt === this.options.stop.maxAttempts || tokenLimitReached) {
        yield {
          type: "completed",
          outcome: {
            outcome: this.options.stop.stopOnFailedVerdict ? "failed_judge" : "exhausted",
            ...base,
            verdict,
          },
        }
        return
      }

      yield { type: "retrying", attempt, verdict }
    }
  }
}
