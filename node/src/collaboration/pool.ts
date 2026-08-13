import type { RuntimeOptions } from "../runtime/runner.js"
import { spawnStandalone } from "../runtime/sub-agent-orchestrator.js"
import type { VerificationContract } from "./contract.js"
import { formatContractForSystemPrompt } from "./contract.js"
import type { AgentRunSpec, KernelAgentRole, SubAgentResult } from "../types/agent.js"
import { agentIdentitySub } from "../types/agent.js"

export type AgentRole = KernelAgentRole

export interface IsolatedVerifierContext {
  contract: VerificationContract
  artifact: string
}

export interface CoordinatorConfig {
  opts: RuntimeOptions
  sessionId: string
}

export interface RoleExecutionInput {
  sessionId: string
  goal: string
  contextInput?: string
  verificationContractId?: string
}

export class AgentPool {
  private coordinator?: CoordinatorConfig

  /** Enable kernel spawn path with lineage recorded under `sessionId`. */
  configureCoordinator(opts: RuntimeOptions, sessionId: string): this {
    this.coordinator = { opts, sessionId }
    return this
  }

  /** Assert that this pool has one canonical kernel coordinator. */
  ensureCoordinator(): this {
    if (!this.coordinator) {
      throw new Error("AgentPool.configureCoordinator() must be called before execution")
    }
    return this
  }

  usesSpawnPath(): boolean {
    return this.coordinator !== undefined
  }

  /** Spawn a kernel-isolated sub-agent. Requires `configureCoordinator()`. */
  async spawn(
    role: KernelAgentRole,
    goal: string,
    extra?: Partial<Omit<AgentRunSpec, "identity" | "role" | "goal">>,
  ): Promise<SubAgentResult> {
    if (!this.coordinator) {
      throw new Error("AgentPool.configureCoordinator() required for kernel spawn path")
    }
    const spec: AgentRunSpec = {
      identity: agentIdentitySub(
        `${role}-${crypto.randomUUID()}`,
        crypto.randomUUID(),
        this.coordinator.sessionId,
      ),
      role,
      goal,
      ...extra,
    }
    return spawnStandalone(this.coordinator.opts, this.coordinator.sessionId, spec)
  }

  /** Execute a role in a caller-owned session while preserving kernel lineage. */
  async execute(role: KernelAgentRole, input: RoleExecutionInput): Promise<SubAgentResult> {
    this.ensureCoordinator()
    const coordinator = this.coordinator!
    const spec: AgentRunSpec = {
      identity: agentIdentitySub(`${role}-${input.sessionId}`, input.sessionId, coordinator.sessionId),
      role,
      goal: input.goal,
      ...(input.verificationContractId
        ? { verificationContractId: input.verificationContractId }
        : {}),
    }
    return spawnStandalone(coordinator.opts, coordinator.sessionId, spec, undefined, input.contextInput)
  }

  async verify(ctx: IsolatedVerifierContext): Promise<string> {
    const contractBlock = formatContractForSystemPrompt(ctx.contract)
    const auditGoal = [
      contractBlock, "",
      "---", "",
      "## Artifact to Audit", "",
      ctx.artifact, "",
      "---", "",
      "Audit the artifact against every criterion in the contract above.",
      "Return only one JSON object with this exact shape:",
      JSON.stringify({
        passed: true,
        overall_score: 1,
        feedback: "overall verification feedback",
        details: ctx.contract.acceptance.map(criterion => ({
          criterion: criterion.id,
          passed: true,
          score: 1,
          feedback: "specific evidence",
        })),
      }, null, 2),
      "Every contract criterion id must appear exactly once in details. Do not emit prose or markdown.",
    ].join("\n")

    const result = await this.spawn("verify", auditGoal, {
      verificationContractId: ctx.contract.id,
      isolation: "read_only",
    })
    return result.result.finalMessage?.content ?? ""
  }

  async orchestrate(goal: string): Promise<string> {
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

    const result = await this.spawn("plan", orchestratorGoal)
    return result.result.finalMessage?.content ?? ""
  }
}
