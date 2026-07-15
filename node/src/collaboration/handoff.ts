import type { ContractCheckResult, VerificationContract } from "./contract.js"

/**
 * HandoffArtifact — the single exchange token between sprints and agent instances.
 *
 * All handoff paths converge here:
 *   - CreatorVerifier AttemptLoop completion → HandoffBus.fromContractOutcome()
 *   - Sub-agent completion              → HandoffBus.fromSubAgentResult()
 *   - Context renewal (kernel)          → carried in kernel's HandoffArtifact type
 *
 * The invariant: a HandoffArtifact tells the next agent not only *what was done*
 * but *what has been proven*. The contract_status field is never discarded on renewal.
 */
export interface HandoffArtifact {
  goal: string
  sprint: number
  progressSummary: string
  openTasks: string[]
  /** Per-criterion verification results from the most recent contract run. */
  contractStatus: ContractCheckResult[]
  /** Ratio of verification failures over 24 h (failed / total). 0.0 if no data. */
  driftRate24h: number
  /** Issues blocking completion — require human or orchestrator attention. */
  blockedOn: string[]
}

/** Input to HandoffBus.fromContractOutcome */
export interface ContractOutcomeInput {
  contract: VerificationContract
  checkResults: ContractCheckResult[]
  artifact: string
  success: boolean
  blockedOn?: string[]
}

/**
 * HandoffBus — the canonical factory for HandoffArtifact.
 *
 * Every transition between agent contexts goes through one of these static methods.
 * This ensures that the resulting artifact always carries contract_status, never just
 * a prose summary.
 */
export class HandoffBus {
  /**
   * Build a HandoffArtifact from a creator-verifier AttemptLoop outcome.
   * The artifact field is used as the progress summary.
   */
  static fromContractOutcome(input: ContractOutcomeInput): HandoffArtifact {
    const failedRequired = input.checkResults.filter(r => {
      if (r.passed) return false
      const c = input.contract.acceptance.find(a => a.id === r.criterionId)
      return c?.required ?? false
    })

    return {
      goal: input.contract.goal,
      sprint: 1,
      progressSummary: input.success
        ? `Completed: ${input.artifact.slice(0, 200)}${input.artifact.length > 200 ? "…" : ""}`
        : `Incomplete after max attempts. ${failedRequired.length} required criteria failed.`,
      openTasks: input.success ? [] : failedRequired.map(r => `Fix criterion: ${r.criterionId}`),
      contractStatus: input.checkResults,
      driftRate24h: input.checkResults.length > 0
        ? input.checkResults.filter(r => !r.passed).length / input.checkResults.length
        : 0,
      blockedOn: input.blockedOn ?? [],
    }
  }

  /**
   * Build a HandoffArtifact from a sub-agent's final message.
   * Matches the convention established by `report_sub_agent` (only final message injected).
   */
  static fromSubAgentResult(opts: {
    goal: string
    finalMessage: string
    sprint?: number
  }): HandoffArtifact {
    return {
      goal: opts.goal,
      sprint: opts.sprint ?? 1,
      progressSummary: opts.finalMessage.slice(0, 500),
      openTasks: [],
      contractStatus: [],
      driftRate24h: 0,
      blockedOn: [],
    }
  }

  /**
   * Render the artifact as a compact injection string for the next agent's
   * working partition (not system — this is a handoff note, not a permanent rule).
   */
  static toContextNote(artifact: HandoffArtifact): string {
    const lines = [
      `[Handoff from sprint ${artifact.sprint}]`,
      `Goal: ${artifact.goal}`,
      `Progress: ${artifact.progressSummary}`,
    ]

    if (artifact.openTasks.length > 0) {
      lines.push(`Open tasks: ${artifact.openTasks.join("; ")}`)
    }

    if (artifact.contractStatus.length > 0) {
      const passed = artifact.contractStatus.filter(r => r.passed).length
      lines.push(`Contract: ${passed}/${artifact.contractStatus.length} criteria passed`)
    }

    if (artifact.blockedOn.length > 0) {
      lines.push(`BLOCKED ON: ${artifact.blockedOn.join("; ")}`)
    }

    if (artifact.driftRate24h > 0) {
      lines.push(`Drift rate: ${(artifact.driftRate24h * 100).toFixed(1)}%`)
    }

    return lines.join("\n")
  }

  /**
   * True when drift rate exceeds threshold or required criteria are blocked.
   * Use to decide whether to pause autonomous delegation and escalate.
   */
  static requiresEscalation(
    artifact: HandoffArtifact,
    opts: { driftThreshold?: number } = {},
  ): boolean {
    const threshold = opts.driftThreshold ?? 0.05
    return artifact.driftRate24h > threshold || artifact.blockedOn.length > 0
  }
}
