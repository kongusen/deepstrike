/**
 * `judge()` — one-shot quality scoring against a goal + criteria using the kernel's `gen_eval`.
 *
 * Wraps the three kernel free functions `buildEvalMessages` / `parseVerdict` / `verdictOutputSchema`
 * (folded out of the old EvalPipeline class in 0.5.0) into a small typed surface that's safe to
 * call from a benchmark harness, a CI gate, or any caller that just wants "does this result meet
 * the criteria?" without setting up `AttemptLoop`.
 *
 * The judge is a single LLM call: build the eval prompt → stream → parse verdict. No retry loop,
 * no skill extraction, no loop state. Use `AttemptLoop` if you want the retry/refine flow.
 */

import type { LLMProvider, Message, RenderedContext, TextDelta } from "../types.js"
import { getKernel } from "../kernel.js"

export interface Criterion {
  /** The criterion text the judge evaluates against. */
  text: string
  /** When true (default), failing this criterion fails the overall verdict. */
  required?: boolean
  /** Optional weight for weighted scoring (kernel-defined semantics). */
  weight?: number
  /** Stable host contract identifier, passed through to deterministic judges. */
  id?: string
  /** Signals that the host can evaluate this criterion without an LLM. */
  machineCheckable?: boolean
}

export interface VerdictDetail {
  criterion: string
  passed: boolean
  score: number
  feedback: string
}

export interface Verdict {
  passed: boolean
  /** 0..1 — kernel-defined aggregate score. */
  overallScore: number
  feedback: string
  details: VerdictDetail[]
}

export interface JudgeArgs {
  /** Provider used for the eval LLM call. Often a cheaper model than the main run. */
  provider: LLMProvider
  /** The task goal the result is being evaluated against. */
  goal: string
  /** The criteria the judge scores against. */
  criteria: Criterion[]
  /** The agent's result text (final reply, or a structured summary when the run was incomplete). */
  result: string
  /** Optional abort signal forwarded to provider.stream. */
  signal?: AbortSignal
}

/**
 * Build the kernel's eval prompt for (goal, criteria, result).
 * Exposed in case a caller wants to render the prompt without calling the LLM (e.g., dry-run cost
 * estimation, fixture generation). For the common case, use `judge()`.
 */
export function buildEvalMessages(goal: string, criteria: Criterion[], result: string): Message[] {
  return getKernel().buildEvalMessages(
    goal,
    criteria.map(c => ({ text: c.text, required: c.required ?? true, weight: c.weight })),
    result,
    1,     // attempt
    false, // extractSkillOnPass — not relevant for benchmark scoring
  )
}

/** Parse a Verdict from raw judge-LLM text. Throws on schema mismatch. */
export function parseVerdict(text: string): Verdict {
  const v = getKernel().parseVerdict(text)
  return {
    passed: v.passed,
    overallScore: v.overallScore,
    feedback: v.feedback,
    details: v.details ?? [],
  }
}

/** The JSON Schema the kernel expects judge output to conform to. */
export function verdictOutputSchema(): Record<string, unknown> {
  return JSON.parse(getKernel().verdictOutputSchema(false)) as Record<string, unknown>
}

/**
 * Run one judge pass: render the eval prompt, stream the provider, parse the verdict.
 * Throws when the provider returns no text or returns content that fails verdict parsing.
 */
export async function judge(args: JudgeArgs): Promise<Verdict> {
  const msgs = buildEvalMessages(args.goal, args.criteria, args.result)
  const ctx: RenderedContext = {
    systemText: msgs.filter(m => m.role === "system").map(m => m.content).join("\n\n"),
    turns: msgs.filter(m => m.role !== "system"),
  }
  let text = ""
  for await (const evt of args.provider.stream(ctx, [], undefined, undefined, args.signal)) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
  }
  if (!text) {
    throw new Error("judge: provider produced no text")
  }
  return parseVerdict(text)
}
