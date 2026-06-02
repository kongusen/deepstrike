import type { AgentPool } from "../pool.js"
import type { VerificationContract } from "../contract.js"
import type { ContractOutcome } from "../harness.js"
import { ContractDrivenHarness } from "../harness.js"
import type { HandoffArtifact } from "../handoff.js"
import { HandoffBus } from "../handoff.js"

export interface CreatorVerifierMetrics {
  total: number
  failed: number
  driftRate: number
}

/**
 * CreatorVerifierMode — the simplest multi-agent collaboration pattern.
 *
 * By default uses the kernel spawn path via `pool.ensureCoordinator()`.
 * Pass `useLegacyRunners: true` to fall back to independent runner sessions.
 *
 * Usage:
 * ```ts
 * const pool = new AgentPool()
 *   .add("executor", executorRunner)
 *   .add("verifier", verifierRunner)
 *
 * const mode = new CreatorVerifierMode(pool)
 * const result = await mode.run(contract)
 *
 * if (HandoffBus.requiresEscalation(result.handoff)) {
 *   // drift > 5% or blocked_on is non-empty → pause autonomous delegation
 * }
 * ```
 */
export class CreatorVerifierMode {
  private _total = 0
  private _failed = 0

  constructor(
    private pool: AgentPool,
    private options: {
      maxAttempts?: number
      /** Stable orchestration session for kernel lineage audit. */
      coordinatorSessionId?: string
    } = {},
  ) {}

  async run(contract: VerificationContract): Promise<ContractOutcome> {
    this._total++

    this.pool.ensureCoordinator(this.options.coordinatorSessionId)

    const harness = new ContractDrivenHarness(this.pool, contract, {
      maxAttempts: this.options.maxAttempts ?? 3,
    })

    const outcome = await harness.run()

    if (!outcome.success) {
      this._failed++
    }

    return outcome
  }

  /** Aggregate drift metrics across all runs through this mode instance. */
  getMetrics(): CreatorVerifierMetrics {
    return {
      total: this._total,
      failed: this._failed,
      driftRate: this._total > 0 ? this._failed / this._total : 0,
    }
  }

  /**
   * True when accumulated drift rate exceeds threshold.
   * When true, pause autonomous delegation and surface to human or orchestrator.
   */
  isDrifting(threshold = 0.05): boolean {
    return this.getMetrics().driftRate > threshold
  }

  /** Reset accumulated metrics (e.g. at the start of a new sprint). */
  resetMetrics(): void {
    this._total = 0
    this._failed = 0
  }
}

/**
 * OrchestrationMode — three-role collaboration: orchestrator → executor → verifier.
 *
 * The orchestrator produces a VerificationContract from a raw goal, then
 * CreatorVerifierMode executes it.
 *
 * Requires all three roles in the pool: orchestrator, executor, verifier.
 */
export class OrchestrationMode {
  private inner: CreatorVerifierMode

  constructor(
    private pool: AgentPool,
    private options: { maxAttempts?: number; coordinatorSessionId?: string } = {},
  ) {
    this.inner = new CreatorVerifierMode(pool, options)
  }

  async run(goal: string): Promise<ContractOutcome & { contract: VerificationContract }> {
    this.pool.ensureCoordinator(this.options.coordinatorSessionId)
    // Step 1: orchestrator produces a VerificationContract
    const contractJson = await this.pool.orchestrate(goal)
    const contract = this._parseContract(contractJson, goal)

    // Step 2: CreatorVerifierMode executes against the contract
    const outcome = await this.inner.run(contract)
    return { ...outcome, contract }
  }

  getMetrics(): CreatorVerifierMetrics {
    return this.inner.getMetrics()
  }

  isDrifting(threshold = 0.05): boolean {
    return this.inner.isDrifting(threshold)
  }

  private _parseContract(json: string, fallbackGoal: string): VerificationContract {
    try {
      // Extract JSON block from markdown fences if present
      const match = json.match(/```(?:json)?\s*([\s\S]*?)```/) ?? [null, json]
      const raw = JSON.parse(match[1] ?? json)
      return {
        id: raw.id ?? "orchestrated",
        goal: raw.goal ?? fallbackGoal,
        acceptance: (raw.acceptance ?? []).map((c: Record<string, unknown>) => ({
          id: String(c.id ?? "criterion"),
          text: String(c.text ?? ""),
          required: c.required !== false,
          weight: Number(c.weight ?? 1.0),
          machineCheckable: Boolean(c.machineCheckable ?? false),
        })),
        antiPatterns: (raw.antiPatterns ?? []).map(String),
        evidenceRequired: (raw.evidenceRequired ?? []).map(String),
      }
    } catch {
      // Orchestrator produced unparseable output — return a minimal contract
      return {
        id: "fallback",
        goal: fallbackGoal,
        acceptance: [{ id: "complete", text: "Goal is satisfactorily completed", required: true, weight: 1.0, machineCheckable: false }],
        antiPatterns: [],
        evidenceRequired: [],
      }
    }
  }
}
