import type { RuntimeRunner } from "../runtime/runner.js"
import { collectText } from "../runtime/runner.js"
import type { DoneEvent, StreamEvent, TextDelta } from "../types.js"
import { writeFile } from "fs/promises"
import path from "path"
import { getKernel } from "../kernel.js"

export interface Criterion {
  text: string
  required: boolean
  weight?: number
  /** I3.3 (A4): optional stable identifier from the host's contract layer (e.g. an
   *  `acceptance[].id` field). The harness does not interpret it; it just threads it through to
   *  `verdictFn` so the host can dispatch per-criterion deterministic checks by id. */
  id?: string
  /** I3.3 (A4): host hint — when true, the host has a deterministic check for this criterion
   *  and would short-circuit the LLM eval. The harness still defers to the host's `verdictFn`
   *  for the actual decision; this is purely a transparency field on the request. */
  machineCheckable?: boolean
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
  /** R3-1: nodes the agent submitted via `submit_workflow_nodes` while running under the harness. */
  submittedNodes?: import("../types/agent.js").WorkflowNodeSpec[]
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
  | { type: "tool_delta"; callId: string; delta?: string; chunk?: Record<string, unknown> }
  | { type: "tool_suspend"; callId: string; suspensionId: string; payload?: Record<string, unknown> }
  | { type: "tool_result"; callId: string; content: string; isError: boolean }
  | { type: "workflow_nodes_submitted"; nodes: import("../types/agent.js").WorkflowNodeSpec[] }
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

export interface QualityGate {
  evaluate(request: HarnessRequest, outcome: HarnessOutcome): Promise<boolean>
}

export class SinglePassHarness {
  constructor(private runner: RuntimeRunner) {}
  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    return { ...await runOnce(this.runner, request), passed: true }
  }
  async *stream(request: HarnessRequest): AsyncIterable<StreamEvent> {
    yield* this.runner.run({ sessionId: crypto.randomUUID(), goal: request.goal, criteria: request.criteria?.map(c => c.text), extensions: request.extensions })
  }
}

/**
 * @deprecated I3.4 (A1): prefer {@link HarnessLoop} with `verdictFn` for host-defined judgment.
 * `EvalLoopHarness.stream()` does NOT honor the `gate` passed via `request.gate` (only `.run()`
 * does), which is a long-standing footgun for streaming hosts. `HarnessLoop` runs the eval loop
 * uniformly across both stream and run, accepts an optional `verdictFn` for short-circuiting the
 * built-in LLM eval, and otherwise mirrors `EvalLoopHarness`'s behavior. New code should use
 * `HarnessLoop`; existing call sites can migrate by switching the class + (if applicable)
 * passing a `verdictFn` for the same gate logic. Slated for removal in a future major. */
export class EvalLoopHarness {
  constructor(private runner: RuntimeRunner, private gate: QualityGate, private maxAttempts = 3) {}

  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    let outcome: HarnessOutcome = { result: "", passed: false, iterations: 0, totalTokens: 0, status: "error" }
    for (let i = 0; i < this.maxAttempts; i++) {
      outcome = await runOnce(this.runner, request)
      if (await this.gate.evaluate(request, outcome)) return { ...outcome, passed: true }
    }
    return outcome
  }
  async *stream(request: HarnessRequest): AsyncIterable<StreamEvent> {
    yield* this.runner.run({ sessionId: crypto.randomUUID(), goal: request.goal, criteria: request.criteria?.map(c => c.text), extensions: request.extensions })
  }
}

/** I3.2 (A2/A3): host-supplied judgment for each attempt's result. Returning a `Verdict` short-
 *  circuits the built-in LLM eval (no `evalProvider.stream` call); returning `undefined` defers to
 *  the built-in eval (enables hybrid judgment: machine-checkable items deterministic, subjective
 *  items LLM). Pure addition — when not set, HarnessLoop.stream() is byte-equivalent to its prior
 *  behavior. The closure owns its own context (doc reader, deterministic checks, etc.); the SDK
 *  is intentionally agnostic about what it inspects. */
export type VerdictFn = (ctx: {
  goal: string
  criteria: Criterion[]
  attempt: number
  result: string
}) => Verdict | undefined | Promise<Verdict | undefined>

export interface HarnessLoopOptions {
  maxAttempts?: number
  skillDir?: string
  verdictFn?: VerdictFn
}

export class HarnessLoop {
  private maxAttempts: number
  private skillDir?: string
  private verdictFn?: VerdictFn

  constructor(
    private runner: RuntimeRunner,
    private evalProvider: import("../types.js").LLMProvider,
    options: HarnessLoopOptions = {},
  ) {
    this.maxAttempts = options.maxAttempts ?? 3
    this.skillDir = options.skillDir
    this.verdictFn = options.verdictFn
  }

  async run(request: HarnessRequest): Promise<HarnessOutcome> {
    let last: HarnessEvent | undefined
    // R3-1: collect nodes the agent submitted while running under the harness, so dynamic fan-out
    // works in harness mode too (not just the plain streaming path).
    const submittedNodes: import("../types/agent.js").WorkflowNodeSpec[] = []
    for await (const evt of this.stream(request)) {
      last = evt
      if (evt.type === "workflow_nodes_submitted") submittedNodes.push(...evt.nodes)
    }
    const done = last?.type === "done" ? last as Extract<HarnessEvent, { type: "done" }> : undefined
    return {
      result: "",
      passed: done?.verdict.passed ?? false,
      iterations: done?.iterations ?? 0,
      totalTokens: done?.totalTokens ?? 0,
      status: done?.status ?? "error",
      overallScore: done?.verdict.overallScore,
      feedback: done?.verdict.feedback,
      details: done?.verdict.details,
      ...(submittedNodes.length ? { submittedNodes } : {}),
    }
  }

  async *stream(request: HarnessRequest): AsyncIterable<HarnessEvent> {
    const kernel = getKernel()
    const criteria = request.criteria ?? []

    let currentGoal = request.goal
    let lastIterations = 0
    let lastTotalTokens = 0
    let lastStatus = "error"
    let lastResult = ""

    for (let attempt = 1; attempt <= this.maxAttempts; attempt++) {
      const sessionId = crypto.randomUUID()
      for await (const evt of this.runner.run({ sessionId, goal: currentGoal, criteria: criteria.map(c => c.text), extensions: request.extensions })) {
        if (evt.type === "text_delta") {
          lastResult += (evt as TextDelta).delta
          yield { type: "token", text: (evt as TextDelta).delta }
        } else if (evt.type === "tool_call") {
          const tc = evt as unknown as { id: string; name: string }
          yield { type: "tool_call", id: tc.id, name: tc.name }
        } else if (evt.type === "tool_delta") {
          const td = evt as unknown as { callId: string; delta?: string; chunk?: Record<string, unknown> }
          yield { type: "tool_delta", callId: td.callId, ...(td.delta ? { delta: td.delta } : {}), ...(td.chunk ? { chunk: td.chunk } : {}) }
        } else if (evt.type === "tool_suspend") {
          const ts = evt as unknown as { callId: string; suspensionId: string; payload?: Record<string, unknown> }
          yield { type: "tool_suspend", callId: ts.callId, suspensionId: ts.suspensionId, ...(ts.payload ? { payload: ts.payload } : {}) }
        } else if (evt.type === "tool_result") {
          const tr = evt as unknown as { callId: string; content: string; isError: boolean }
          yield { type: "tool_result", callId: tr.callId, content: tr.content, isError: tr.isError }
        } else if (evt.type === "workflow_nodes_submitted") {
          const ws = evt as unknown as { nodes: import("../types/agent.js").WorkflowNodeSpec[] }
          yield { type: "workflow_nodes_submitted", nodes: ws.nodes }
        } else if (evt.type === "done") {
          const d = evt as DoneEvent
          lastIterations = d.iterations
          lastTotalTokens = d.totalTokens
          lastStatus = d.status
        }
      }

      yield { type: "supervising" }

      // I3.2 (A2/A3): host-supplied `verdictFn` short-circuits the LLM eval. When it returns a
      // Verdict, use it; when it returns undefined, defer to the built-in eval (hybrid path).
      let verdict: Verdict | undefined
      let skillCandidate: ReturnType<typeof kernel.parseVerdict>["skillCandidate"]
      if (this.verdictFn) {
        verdict = await this.verdictFn({ goal: request.goal, criteria, attempt, result: lastResult })
      }
      if (!verdict) {
        // #6 (0.5.0): the eval/verdict compute is the kernel's stateless free functions (was the
        // EvalPipeline state machine). Build the eval prompt, call the eval LLM, parse the verdict.
        const evalMsgs = kernel.buildEvalMessages(request.goal, criteria, lastResult, attempt, true)
        let evalText = ""
        const evalContext = {
          systemText: evalMsgs.filter((m: { role: string }) => m.role === "system").map((m: { content: string }) => m.content).join("\n\n"),
          turns: evalMsgs.filter((m: { role: string }) => m.role !== "system"),
        }
        for await (const evt of this.evalProvider.stream(evalContext, [], undefined)) {
          if (evt.type === "text_delta") evalText += (evt as TextDelta).delta
        }
        const parsed = kernel.parseVerdict(evalText)
        verdict = {
          passed: parsed.passed,
          overallScore: parsed.overallScore,
          feedback: parsed.feedback,
          details: parsed.details ?? [],
        }
        skillCandidate = parsed.skillCandidate
      }

      if (verdict.passed) {
        if (skillCandidate && this.skillDir) {
          const { name, description, whenToUse, content } = skillCandidate
          const fm = ["---", `name: ${name}`, `description: ${description}`,
            whenToUse ? `when_to_use: ${whenToUse}` : null, "---", ""]
            .filter(Boolean).join("\n")
          await writeFile(path.join(this.skillDir, `${name}.md`), fm + content, "utf8")
        }
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

// Re-export collectText so harness callers can use it without knowing runner internals.
export { collectText }
