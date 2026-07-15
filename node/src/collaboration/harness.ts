import type { AttemptBody, AttemptBodyContext, AttemptBodyEvent } from "../harness/harness.js"
import type { AttemptJudge, JudgeContext, JudgeResult } from "../harness/judge.js"
import { parseVerdict } from "../runtime/eval.js"
import type { ContractCheckResult, VerificationContract } from "./contract.js"
import { formatContractForSystemPrompt } from "./contract.js"
import type { HandoffArtifact } from "./handoff.js"
import type { AgentPool } from "./pool.js"

export interface ContractOutcome {
  success: boolean
  artifact: string
  checkResults: ContractCheckResult[]
  attemptsUsed: number
  totalTokensConsumed: number
  handoff: HandoffArtifact
}

/** The creator-verifier body owns execution only; verification is an AttemptJudge. */
export class CreatorVerifierBody implements AttemptBody {
  constructor(
    private readonly pool: AgentPool,
    private readonly contract: VerificationContract,
  ) {}

  async *run(context: AttemptBodyContext): AsyncIterable<AttemptBodyEvent> {
    const contractBlock = formatContractForSystemPrompt(this.contract)
    const result = await this.pool.execute("executor", {
      sessionId: context.sessionId,
      goal: `${contractBlock}\n\n---\n\n${context.goal}`,
      ...(context.contextInput ? { contextInput: context.contextInput } : {}),
      verificationContractId: this.contract.id,
    })
    const artifact = result.result.finalMessage?.content ?? ""
    if (artifact) yield { type: "token", text: artifact }
    yield {
      type: "body_done",
      runStatus: String(result.result.termination),
      result: artifact,
      turns: result.result.turnsUsed,
      totalTokens: result.result.totalTokensUsed,
      ...(result.submittedNodes?.length ? { submittedNodes: result.submittedNodes } : {}),
    }
  }
}

/** Structured verifier output only. Free-text PASS/FAIL inference is intentionally unsupported. */
export class StructuredContractJudge implements AttemptJudge {
  constructor(
    private readonly pool: AgentPool,
    private readonly contract: VerificationContract,
  ) {}

  async judge(context: JudgeContext): Promise<JudgeResult> {
    const auditText = await this.pool.verify({ contract: this.contract, artifact: context.result })
    const wire = JSON.parse(auditText) as Record<string, unknown>
    if (typeof wire !== "object" || wire === null || Array.isArray(wire)) {
      throw new Error("structured verifier output must be a JSON object")
    }
    const parsed = parseVerdict(auditText)
    const details = this.contract.acceptance.map(criterion => {
      const detail = parsed.details.find(candidate =>
        candidate.criterion === criterion.id || candidate.criterion === criterion.text)
      return detail
        ? { ...detail, criterion: criterion.id }
        : {
            criterion: criterion.id,
            passed: false,
            score: 0,
            feedback: "criterion missing from structured verifier output",
          }
    })
    const requiredPassed = this.contract.acceptance.every((criterion, index) =>
      !criterion.required || details[index]!.passed)
    return {
      verdict: {
        passed: parsed.passed && requiredPassed,
        overallScore: parsed.overallScore,
        feedback: parsed.feedback,
        details,
      },
    }
  }
}
