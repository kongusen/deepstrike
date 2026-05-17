import type { AgentPool } from "./pool.js"
import type { VerificationContract, ContractCheckResult } from "./contract.js"
import { formatContractForSystemPrompt, contractToCriteriaStrings } from "./contract.js"
import type { HandoffArtifact } from "./handoff.js"
import { HandoffBus } from "./handoff.js"

export interface Violation {
  criterionId: string
  text: string
  detail: string
}

export interface ContractOutcome {
  success: boolean
  artifact: string
  checkResults: ContractCheckResult[]
  attemptsUsed: number
  totalTokensConsumed: number
  handoff: HandoffArtifact
}

export interface ContractHarnessOptions {
  maxAttempts?: number
  onViolation?: (violations: Violation[]) => void
}

/**
 * ContractDrivenHarness — the core multi-agent execution primitive.
 *
 * Differs from HarnessLoop in three ways:
 *   1. Executor and verifier are **separate Agent instances** — no shared history.
 *   2. Verifier receives only the artifact + contract, not the implementation transcript.
 *   3. Feedback returned to the executor is a structured list of Violations,
 *      not a free-text LLM summary.
 *
 * Protocol per attempt:
 *   executor.run(goal, contract) → artifact
 *   verifier.runIsolated(artifact, contract) → audit text
 *   parse audit text → ContractCheckResult[]
 *   all required criteria pass → Done
 *   violations remain → inject only violation list into next executor goal
 *   maxAttempts exceeded → produce HandoffArtifact with blocked_on
 */
export class ContractDrivenHarness {
  private maxAttempts: number
  private onViolation?: (violations: Violation[]) => void

  constructor(
    private pool: AgentPool,
    private contract: VerificationContract,
    options: ContractHarnessOptions = {},
  ) {
    this.maxAttempts = options.maxAttempts ?? 3
    this.onViolation = options.onViolation
  }

  async run(): Promise<ContractOutcome> {
    let artifact = ""
    let checkResults: ContractCheckResult[] = []
    let attemptsUsed = 0
    let currentGoal = this.contract.goal

    for (let attempt = 1; attempt <= this.maxAttempts; attempt++) {
      attemptsUsed = attempt

      // ── Phase 1: Executor ──────────────────────────────────────────────────
      // Executor sees: contract block + goal. No verifier history.
      const contractBlock = formatContractForSystemPrompt(this.contract)
      const violationNote = attempt > 1
        ? `\n\n[Previous attempt failed. Violations to fix:\n${this._formatViolationsForFeedback(checkResults)}]`
        : ""
      const executorGoal = `${contractBlock}\n\n---\n\n${currentGoal}${violationNote}`

      artifact = await this.pool.get("executor").run(
        executorGoal,
        contractToCriteriaStrings(this.contract),
      )

      // ── Phase 2: Verifier ──────────────────────────────────────────────────
      // Verifier sees: artifact + contract only. No executor history.
      const auditText = await this.pool.runVerifier({ contract: this.contract, artifact })

      // ── Phase 3: Parse audit → ContractCheckResult[] ──────────────────────
      checkResults = this._parseAuditText(auditText)

      const violations = this._findViolations(checkResults)

      if (violations.length === 0) {
        // All required criteria passed
        return {
          success: true,
          artifact,
          checkResults,
          attemptsUsed,
          totalTokensConsumed: 0,
          handoff: HandoffBus.fromContractOutcome({
            contract: this.contract,
            checkResults,
            artifact,
            success: true,
          }),
        }
      }

      this.onViolation?.(violations)
    }

    // Max attempts exhausted
    const blockedOn = this._findViolations(checkResults).map(v =>
      `[${v.criterionId}] ${v.text}: ${v.detail}`,
    )

    return {
      success: false,
      artifact,
      checkResults,
      attemptsUsed,
      totalTokensConsumed: 0,
      handoff: HandoffBus.fromContractOutcome({
        contract: this.contract,
        checkResults,
        artifact,
        success: false,
        blockedOn,
      }),
    }
  }

  private _findViolations(results: ContractCheckResult[]): Violation[] {
    const violations: Violation[] = []
    for (const result of results) {
      if (!result.passed) {
        const criterion = this.contract.acceptance.find(c => c.id === result.criterionId)
        if (criterion?.required) {
          violations.push({
            criterionId: result.criterionId,
            text: criterion.text,
            detail: result.evidence ?? "no evidence provided",
          })
        }
      }
    }
    return violations
  }

  private _formatViolationsForFeedback(results: ContractCheckResult[]): string {
    return this._findViolations(results)
      .map(v => `- [${v.criterionId}] ${v.text}: ${v.detail}`)
      .join("\n")
  }

  /**
   * Parse the verifier's free-text audit into structured ContractCheckResult[].
   *
   * The verifier is prompted to produce a structured PASS/FAIL per criterion.
   * This parser handles the common patterns; callers can subclass and override
   * for stricter parsing.
   */
  private _parseAuditText(auditText: string): ContractCheckResult[] {
    const results: ContractCheckResult[] = []
    const lower = auditText.toLowerCase()

    for (const criterion of this.contract.acceptance) {
      // Look for explicit "id: PASS" or "id: FAIL" patterns from the verifier prompt
      const idPattern = new RegExp(
        `\\b${criterion.id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b[^\\n]*?(pass|fail)`,
        "i",
      )
      const match = auditText.match(idPattern)

      if (match) {
        const passed = match[1].toLowerCase() === "pass"
        // Extract the line as evidence
        const lineStart = auditText.lastIndexOf("\n", match.index ?? 0) + 1
        const lineEnd = auditText.indexOf("\n", match.index ?? 0)
        const evidence = auditText.slice(lineStart, lineEnd > 0 ? lineEnd : undefined).trim()
        results.push({ criterionId: criterion.id, passed, evidence })
      } else {
        // Fallback: search for criterion text near PASS/FAIL keywords
        const textIdx = lower.indexOf(criterion.text.toLowerCase().slice(0, 30))
        if (textIdx !== -1) {
          const window = lower.slice(textIdx, textIdx + 200)
          const passed = window.includes("pass") && !window.includes("fail")
          results.push({ criterionId: criterion.id, passed, evidence: "inferred from context" })
        } else {
          // No mention found — conservative: treat as failed
          results.push({
            criterionId: criterion.id,
            passed: false,
            evidence: "criterion not mentioned in audit",
          })
        }
      }
    }

    return results
  }
}
