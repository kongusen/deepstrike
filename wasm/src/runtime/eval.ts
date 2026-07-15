/**
 * `judge()` — one-shot quality scoring against a goal + criteria using the kernel's `gen_eval`.
 *
 * Mirrors the Node SDK's `node/src/runtime/eval.ts`. WASM port differences:
 *   - The kernel is loaded asynchronously via `getKernel()`, so the wrapper functions are async
 *     where the Node versions are sync. `judge()` was already async on both ports.
 *
 * See node/src/runtime/eval.ts for the full design rationale.
 */

import type { LLMProvider, Message, RenderedContext, StreamEvent, TextDelta } from "../types.js"
import { getKernel } from "./kernel.js"

export interface Criterion {
  text: string
  required?: boolean
  weight?: number
  id?: string
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
  overallScore: number
  feedback: string
  details: VerdictDetail[]
}

export interface JudgeArgs {
  provider: LLMProvider
  goal: string
  criteria: Criterion[]
  result: string
  signal?: AbortSignal
}

/** Build the kernel's eval prompt for (goal, criteria, result). Async — the WASM kernel loads lazily. */
export async function buildEvalMessages(
  goal: string,
  criteria: Criterion[],
  result: string,
): Promise<Message[]> {
  const kernel = await getKernel()
  return kernel.buildEvalMessages(
    goal,
    criteria.map(c => ({ text: c.text, required: c.required ?? true, weight: c.weight })),
    result,
    1,
    false,
  )
}

/** Parse a Verdict from raw judge-LLM text. */
export async function parseVerdict(text: string): Promise<Verdict> {
  const kernel = await getKernel()
  const v = kernel.parseVerdict(text)
  return {
    passed: v.passed,
    overallScore: v.overallScore,
    feedback: v.feedback,
    details: (v.details ?? []) as VerdictDetail[],
  }
}

/** The JSON Schema the kernel expects judge output to conform to. */
export async function verdictOutputSchema(): Promise<Record<string, unknown>> {
  const kernel = await getKernel()
  return JSON.parse(kernel.verdictOutputSchema(false)) as Record<string, unknown>
}

/**
 * Run one judge pass: render the eval prompt, stream the provider, parse the verdict.
 * Throws when the provider returns no text.
 */
export async function judge(args: JudgeArgs): Promise<Verdict> {
  const msgs = await buildEvalMessages(args.goal, args.criteria, args.result)
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
