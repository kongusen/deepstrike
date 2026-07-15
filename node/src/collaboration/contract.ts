/**
 * VerificationContract — first-class type for contract-driven development.
 *
 * Contracts travel in the executor's system partition (never compressed) and
 * are given to the verifier alongside the artifact. The verifier never sees
 * the executor's implementation history — only the goal, contract, and artifact.
 */

export interface AcceptanceCriterion {
  /** Stable id — matched in ContractCheckResult. */
  id: string
  /** Human-readable statement of what correct looks like. */
  text: string
  /** If true, failure here fails the entire contract regardless of score. */
  required: boolean
  /** Contribution to the weighted overall score [0.0–1.0]. */
  weight: number
  /** If true the SDK can verify this deterministically (e.g. word count, schema). */
  machineCheckable: boolean
}

export interface VerificationContract {
  /** Stable id — doubles as the skill name on successful extraction. */
  id: string
  /** Goal this contract governs. Injected into the executor's context. */
  goal: string
  acceptance: AcceptanceCriterion[]
  /** Patterns the executor must avoid. Checked by the verifier. */
  antiPatterns: string[]
  /** Artifacts that must be present before the verifier runs. */
  evidenceRequired: string[]
}

export interface ContractCheckResult {
  criterionId: string
  passed: boolean
  evidence?: string
}

/** Build a contract incrementally. */
export class ContractBuilder {
  private contract: VerificationContract

  constructor(id: string, goal: string) {
    this.contract = { id, goal, acceptance: [], antiPatterns: [], evidenceRequired: [] }
  }

  criterion(
    id: string,
    text: string,
    opts: { required?: boolean; weight?: number; machineCheckable?: boolean } = {},
  ): this {
    this.contract.acceptance.push({
      id,
      text,
      required: opts.required ?? true,
      weight: Math.min(1, Math.max(0, opts.weight ?? 1.0)),
      machineCheckable: opts.machineCheckable ?? false,
    })
    return this
  }

  antiPattern(pattern: string): this {
    this.contract.antiPatterns.push(pattern)
    return this
  }

  evidence(item: string): this {
    this.contract.evidenceRequired.push(item)
    return this
  }

  build(): VerificationContract {
    return structuredClone(this.contract)
  }
}

/**
 * Render the contract as a markdown block for injection into a system prompt.
 * Used by CreatorVerifierBody to inject into the executor's system partition.
 */
export function formatContractForSystemPrompt(contract: VerificationContract): string {
  const lines: string[] = [
    `## Verification Contract: ${contract.id}`,
    "",
    `Goal: ${contract.goal}`,
    "",
    "### Acceptance Criteria",
  ]

  contract.acceptance.forEach((c, i) => {
    const req = c.required ? "[REQUIRED]" : "[OPTIONAL]"
    lines.push(`${i + 1}. ${req} ${c.text} (id: \`${c.id}\`, weight: ${c.weight.toFixed(1)})`)
  })

  if (contract.antiPatterns.length > 0) {
    lines.push("", "### Anti-Patterns (must avoid)")
    contract.antiPatterns.forEach(p => lines.push(`- ${p}`))
  }

  if (contract.evidenceRequired.length > 0) {
    lines.push("", "### Required Evidence")
    contract.evidenceRequired.forEach(e => lines.push(`- ${e}`))
  }

  return lines.join("\n")
}

/** Derive a flat string[] of criterion texts for RuntimeRunner criteria input. */
export function contractToCriteriaStrings(contract: VerificationContract): string[] {
  return contract.acceptance.map(c => c.text)
}
