import type { RuntimeRunner } from "../runtime/runner.js"
import { collectText } from "../runtime/runner.js"
import type { VerificationContract } from "./contract.js"
import { formatContractForSystemPrompt } from "./contract.js"

export type AgentRole = "orchestrator" | "executor" | "verifier"

export interface IsolatedVerifierContext {
  contract: VerificationContract
  artifact: string
}

export class AgentPool {
  private runners = new Map<AgentRole, RuntimeRunner>()

  add(role: AgentRole, runner: RuntimeRunner): this {
    this.runners.set(role, runner)
    return this
  }

  has(role: AgentRole): boolean {
    return this.runners.has(role)
  }

  get(role: AgentRole): RuntimeRunner {
    const runner = this.runners.get(role)
    if (!runner) throw new Error(`AgentPool: no runner registered for role "${role}"`)
    return runner
  }

  async runVerifier(ctx: IsolatedVerifierContext): Promise<string> {
    const runner = this.get("verifier")
    const contractBlock = formatContractForSystemPrompt(ctx.contract)

    const auditGoal = [
      contractBlock, "",
      "---", "",
      "## Artifact to Audit", "",
      ctx.artifact, "",
      "---", "",
      "Audit the artifact against every criterion in the contract above.",
      "For each criterion, state whether it PASSED or FAILED and cite specific evidence.",
      "List any anti-patterns you detected.",
      "Conclude with an overall PASS or FAIL verdict.",
    ].join("\n")

    return collectText(runner.run({ sessionId: crypto.randomUUID(), goal: auditGoal }))
  }

  async runOrchestrator(goal: string): Promise<string> {
    const runner = this.get("orchestrator")
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

    return collectText(runner.run({ sessionId: crypto.randomUUID(), goal: orchestratorGoal }))
  }
}
