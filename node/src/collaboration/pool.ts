import type { RuntimeOptions, RuntimeRunner } from "../runtime/runner.js"
import { collectText } from "../runtime/runner.js"
import { spawnStandalone } from "../runtime/sub-agent-orchestrator.js"
import type { VerificationContract } from "./contract.js"
import { formatContractForSystemPrompt } from "./contract.js"
import type { AgentRunSpec, KernelAgentRole, SubAgentResult } from "../types/agent.js"
import { agentIdentitySub } from "../types/agent.js"
import type { DoneEvent, TextDelta } from "../types.js"

/** Legacy pool roles — mapped to kernel AgentRole when using spawn path. */
export type AgentRole = "orchestrator" | "executor" | "verifier"

export const KERNEL_ROLE_MAP: Record<AgentRole, KernelAgentRole> = {
  orchestrator: "plan",
  executor: "implement",
  verifier: "verify",
}

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
  private runners = new Map<AgentRole, RuntimeRunner>()
  private coordinator?: CoordinatorConfig

  add(role: AgentRole, runner: RuntimeRunner): this {
    this.runners.set(role, runner)
    return this
  }

  /** Enable kernel spawn path with lineage recorded under `sessionId`. */
  configureCoordinator(opts: RuntimeOptions, sessionId: string): this {
    this.coordinator = { opts, sessionId }
    return this
  }

  /**
   * Infer coordinator from a registered runner (executor → orchestrator → verifier).
   * Idempotent when coordinator is already configured.
   */
  ensureCoordinator(sessionId?: string): this {
    if (this.coordinator) return this
    const source: AgentRole = this.has("executor")
      ? "executor"
      : this.has("orchestrator")
        ? "orchestrator"
        : "verifier"
    return this.configureCoordinator(
      this.get(source).hostOptions,
      sessionId ?? crypto.randomUUID(),
    )
  }

  usesSpawnPath(): boolean {
    return this.coordinator !== undefined
  }

  has(role: AgentRole): boolean {
    return this.runners.has(role)
  }

  get(role: AgentRole): RuntimeRunner {
    const runner = this.runners.get(role)
    if (!runner) throw new Error(`AgentPool: no runner registered for role "${role}"`)
    return runner
  }

  /**
   * Spawn a kernel-isolated sub-agent. Requires `configureCoordinator()`.
   * Maps legacy pool roles to kernel roles (executor → implement, etc.).
   */
  async spawn(
    role: AgentRole | KernelAgentRole,
    goal: string,
    extra?: Partial<Omit<AgentRunSpec, "identity" | "role" | "goal">>,
  ): Promise<SubAgentResult> {
    if (!this.coordinator) {
      throw new Error("AgentPool.configureCoordinator() required for kernel spawn path")
    }
    const kernelRole: KernelAgentRole =
      role in KERNEL_ROLE_MAP ? KERNEL_ROLE_MAP[role as AgentRole] : (role as KernelAgentRole)
    const spec: AgentRunSpec = {
      identity: agentIdentitySub(
        `${kernelRole}-${crypto.randomUUID()}`,
        crypto.randomUUID(),
        this.coordinator.sessionId,
      ),
      role: kernelRole,
      goal,
      ...extra,
    }
    return spawnStandalone(this.coordinator.opts, this.coordinator.sessionId, spec)
  }

  /** Execute a role in a caller-owned session so AttemptLoop can retain transcript across attempts. */
  async execute(role: AgentRole, input: RoleExecutionInput): Promise<SubAgentResult> {
    if (this.coordinator) {
      const kernelRole = KERNEL_ROLE_MAP[role]
      const spec: AgentRunSpec = {
        identity: agentIdentitySub(
          `${kernelRole}-${input.sessionId}`,
          input.sessionId,
          this.coordinator.sessionId,
        ),
        role: kernelRole,
        goal: input.goal,
        ...(input.verificationContractId
          ? { verificationContractId: input.verificationContractId }
          : {}),
      }
      return spawnStandalone(
        this.coordinator.opts,
        this.coordinator.sessionId,
        spec,
        undefined,
        input.contextInput,
      )
    }

    const runner = this.get(role)
    if (input.contextInput) runner.injectNote(input.contextInput)
    let finalText = ""
    let turnsUsed = 0
    let totalTokensUsed = 0
    let termination = "error"
    for await (const event of runner.run({ sessionId: input.sessionId, goal: input.goal })) {
      if (event.type === "text_delta") finalText += (event as TextDelta).delta
      if (event.type === "done") {
        const done = event as DoneEvent
        turnsUsed = done.iterations
        totalTokensUsed = done.totalTokens
        termination = done.status
      }
    }
    return {
      agentId: `${KERNEL_ROLE_MAP[role]}-${input.sessionId}`,
      result: {
        termination,
        turnsUsed,
        totalTokensUsed,
        ...(finalText
          ? { finalMessage: { role: "assistant", content: finalText, toolCalls: [] } }
          : {}),
      },
    }
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

    if (this.coordinator) {
      const result = await this.spawn("verify", auditGoal, {
        verificationContractId: ctx.contract.id,
        isolation: "read_only",
      })
      return result.result.finalMessage?.content ?? ""
    }

    const runner = this.get("verifier")
    return collectText(runner.run({ sessionId: crypto.randomUUID(), goal: auditGoal }))
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

    if (this.coordinator) {
      const result = await this.spawn("plan", orchestratorGoal)
      return result.result.finalMessage?.content ?? ""
    }

    return collectText(this.get("orchestrator").run({
      sessionId: crypto.randomUUID(),
      goal: orchestratorGoal,
    }))
  }
}
