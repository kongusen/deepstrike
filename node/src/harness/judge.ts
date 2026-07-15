// AttemptJudge — the "how do we judge one attempt?" Strategy, consumed by AttemptLoop so the
// judgment step is a named, testable, swappable unit rather than inline loop logic.
//
// The built-in strategies are independent of retry/carry behavior:
//   • VerdictFnJudge — host-supplied deterministic judgment; returns `undefined` to defer.
//   • LlmEvalJudge   — the kernel's stateless eval (buildEvalMessages → stream → parseVerdict).
//   • HybridJudge    — try the primary; on `undefined`, fall back (verdictFn ?? LLM-eval).
import type { LLMProvider, TextDelta } from "../types.js"
import { getKernel } from "../kernel.js"
import type { VerdictFn } from "./harness.js"
import type { Criterion, Verdict } from "../runtime/eval.js"

export type SkillCandidate = ReturnType<ReturnType<typeof getKernel>["parseVerdict"]>["skillCandidate"]

export interface JudgeContext {
  goal: string
  criteria: Criterion[]
  attempt: number
  result: string
}

export interface JudgeResult {
  verdict: Verdict
  /** Skill the judge proposes extracting on pass (LLM-eval path only). */
  skillCandidate?: SkillCandidate
}

/** Decides whether one attempt's result meets the criteria. Returning `undefined` defers to a
 *  fallback judge (enables hybrid host/LLM judgment). */
export interface AttemptJudge {
  judge(ctx: JudgeContext): Promise<JudgeResult | undefined>
}

/** Wraps a host-supplied `VerdictFn`. Returns `undefined` (defer) when the function does. */
export class VerdictFnJudge implements AttemptJudge {
  constructor(private readonly fn: VerdictFn) {}
  async judge(ctx: JudgeContext): Promise<JudgeResult | undefined> {
    const verdict = await this.fn({ goal: ctx.goal, criteria: ctx.criteria, attempt: ctx.attempt, result: ctx.result })
    return verdict ? { verdict } : undefined
  }
}

/** The built-in LLM eval: render the kernel eval prompt, stream the eval provider, parse the
 *  verdict. Always produces a JudgeResult (never defers). */
export class LlmEvalJudge implements AttemptJudge {
  constructor(
    private readonly evalProvider: LLMProvider,
    private readonly extractSkillOnPass: boolean = false,
  ) {}

  async judge(ctx: JudgeContext): Promise<JudgeResult> {
    const kernel = getKernel()
    const evalMsgs = kernel.buildEvalMessages(
      ctx.goal,
      ctx.criteria.map(criterion => ({
        text: criterion.text,
        required: criterion.required ?? true,
        weight: criterion.weight,
      })),
      ctx.result,
      ctx.attempt,
      this.extractSkillOnPass,
    )
    const evalContext = {
      systemText: evalMsgs.filter((m: { role: string }) => m.role === "system").map((m: { content: string }) => m.content).join("\n\n"),
      turns: evalMsgs.filter((m: { role: string }) => m.role !== "system"),
    }
    let evalText = ""
    for await (const evt of this.evalProvider.stream(evalContext, [], undefined)) {
      if (evt.type === "text_delta") evalText += (evt as TextDelta).delta
    }
    const parsed = kernel.parseVerdict(evalText)
    return {
      verdict: {
        passed: parsed.passed,
        overallScore: parsed.overallScore,
        feedback: parsed.feedback,
        details: parsed.details ?? [],
      },
      skillCandidate: parsed.skillCandidate,
    }
  }
}

/** Try `primary`; if it defers (`undefined`), use `fallback`.
 *  "verdictFn short-circuits, else built-in LLM eval" hybrid judgment. */
export class HybridJudge implements AttemptJudge {
  constructor(private readonly primary: AttemptJudge, private readonly fallback: AttemptJudge) {}
  async judge(ctx: JudgeContext): Promise<JudgeResult | undefined> {
    return (await this.primary.judge(ctx)) ?? (await this.fallback.judge(ctx))
  }
}
