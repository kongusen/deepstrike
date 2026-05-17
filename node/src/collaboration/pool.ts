import type { Agent } from "../agent.js"
import type { VerificationContract } from "./contract.js"
import { formatContractForSystemPrompt } from "./contract.js"

/**
 * Roles in a multi-agent collaboration.
 *
 * - orchestrator: strong-reasoning, produces VerificationContracts; no tools beyond planning
 * - executor:     code/task execution, full tool access, sees goal + contract only
 * - verifier:     adversarial auditor, no tools, low temperature, sees artifact + contract only
 */
export type AgentRole = "orchestrator" | "executor" | "verifier"

/** Context passed to a verifier run — intentionally minimal. */
export interface IsolatedVerifierContext {
  contract: VerificationContract
  /** The artifact produced by the executor. */
  artifact: string
}

/**
 * AgentPool manages a set of role-specific Agent instances.
 *
 * Each role runs in its own Agent instance with an independent history partition,
 * ensuring that the verifier never sees the executor's implementation transcript.
 *
 * Usage:
 * ```ts
 * const pool = new AgentPool()
 *   .add("executor", executorAgent)
 *   .add("verifier", verifierAgent)
 * ```
 */
export class AgentPool {
  private agents = new Map<AgentRole, Agent>()

  add(role: AgentRole, agent: Agent): this {
    this.agents.set(role, agent)
    return this
  }

  has(role: AgentRole): boolean {
    return this.agents.has(role)
  }

  get(role: AgentRole): Agent {
    const agent = this.agents.get(role)
    if (!agent) throw new Error(`AgentPool: no agent registered for role "${role}"`)
    return agent
  }

  /**
   * Run the verifier with an isolated context — only the artifact and contract.
   * The verifier does NOT receive the executor's conversation history.
   * Returns a structured audit response as a plain string.
   */
  async runVerifier(ctx: IsolatedVerifierContext): Promise<string> {
    const agent = this.get("verifier")
    const contractBlock = formatContractForSystemPrompt(ctx.contract)

    const auditGoal = [
      contractBlock,
      "",
      "---",
      "",
      "## Artifact to Audit",
      "",
      ctx.artifact,
      "",
      "---",
      "",
      "Audit the artifact against every criterion in the contract above.",
      "For each criterion, state whether it PASSED or FAILED and cite specific evidence.",
      "List any anti-patterns you detected.",
      "Conclude with an overall PASS or FAIL verdict.",
    ].join("\n")

    return agent.run(auditGoal)
  }

  /**
   * Run the orchestrator to decompose a high-level goal into a VerificationContract.
   * The orchestrator receives the goal and must produce a structured contract in JSON.
   */
  async runOrchestrator(goal: string): Promise<string> {
    const agent = this.get("orchestrator")
    const orchestratorGoal = [
      `You are a planning orchestrator. Decompose the following goal into a VerificationContract.`,
      ``,
      `Goal: ${goal}`,
      ``,
      `Produce a JSON object with this schema:`,
      `{`,
      `  "id": "<kebab-case-id>",`,
      `  "goal": "<restated goal>",`,
      `  "acceptance": [{ "id": "<id>", "text": "<criterion>", "required": true, "weight": 0.x, "machineCheckable": false }],`,
      `  "antiPatterns": ["<pattern>"],`,
      `  "evidenceRequired": ["<evidence item>"]`,
      `}`,
      ``,
      `Output ONLY the JSON object, no prose.`,
    ].join("\n")

    return agent.run(orchestratorGoal)
  }
}
