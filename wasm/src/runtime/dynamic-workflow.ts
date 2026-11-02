/**
 * Plan task 6.3 · dynamic workflows: a host program decides, step by step, which agents to run.
 *
 * Each `agent` (or `parallelAgents` batch) becomes one submission the kernel admits onto the
 * operation's dynamic workflow root — the kernel still owns the DAG, its budgets and every spawn —
 * and the program awaits the typed result before deciding what comes next. The vocabulary matches
 * the Node SDK's `runDynamicWorkflow`; replay of a saved run and the sandboxed script executors are
 * Node-only (they need a filesystem and `node:vm`).
 */
import type { KernelAgentRole, WorkflowNodeSpec, WorkflowOutcome, WorkflowSpec } from "./types/agent.js"
import { sha256Hex } from "./sha256.js"

export interface DynamicWorkflowLimits {
  maxAgentsPerRun?: number
  maxConcurrentAgents?: number
  maxBatchSize?: number
}

const DEFAULT_LIMITS: Required<DynamicWorkflowLimits> = { maxAgentsPerRun: 64, maxConcurrentAgents: 8, maxBatchSize: 16 }

export interface DynamicWorkflowAgentOptions {
  label?: string
  role?: KernelAgentRole
  modelHint?: string
  trust?: WorkflowNodeSpec["trust"]
  isolation?: WorkflowNodeSpec["isolation"]
  outputSchema?: Record<string, unknown>
  tokenBudget?: number
  maxTurns?: number
  maxWallMs?: number
}

export interface DynamicWorkflowAgentRequest {
  prompt: string
  options?: DynamicWorkflowAgentOptions
}

export interface DynamicWorkflowAgentResult<T = string> {
  value: T
  text: string
  nodeId: string
  status: string
  termination?: string
}

export interface DynamicWorkflowPhaseProgress {
  name: string
  status: "running" | "completed" | "failed"
  agentsStarted: number
  agentsCompleted: number
}

export interface DynamicWorkflowProgress {
  runId: string
  agentsStarted: number
  agentsCompleted: number
  activeAgents: number
  phase?: string
  phases: DynamicWorkflowPhaseProgress[]
  logs: Array<{ message: string; fields?: Record<string, unknown> }>
}

export interface DynamicWorkflowRun<T> {
  runId: string
  value: T
  progress: DynamicWorkflowProgress
}

/** Plan task 6.2 · state the program shares with the agents it runs (see the Node SDK). */
export interface DynamicWorkflowBlackboard {
  put(key: string, value: unknown, options?: { scope?: "workflow" | "phase" }): Promise<void>
  get(key: string): unknown
  delete(key: string): Promise<void>
}

export interface DynamicWorkflowContext<TArgs extends Record<string, unknown> = Record<string, unknown>> {
  readonly args: Readonly<TArgs>
  readonly progress: DynamicWorkflowProgress
  readonly blackboard: DynamicWorkflowBlackboard
  agent<T = string>(prompt: string, options?: DynamicWorkflowAgentOptions): Promise<DynamicWorkflowAgentResult<T> | null>
  parallelAgents<T>(items: readonly T[], worker: (item: T, index: number) => DynamicWorkflowAgentRequest): Promise<Array<DynamicWorkflowAgentResult | null>>
  parallel<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]>
  pipeline<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]>
  phase<T>(name: string, body: () => Promise<T> | T): Promise<T>
  log(message: string, fields?: Record<string, unknown>): void
}

export class DynamicWorkflowLimitError extends Error {
  readonly code = "dynamic_workflow_limit" as const
}

/** What the context needs from the runner: admit and drive one submission, write the blackboard. */
export interface DynamicWorkflowHost {
  runWorkflow(spec: WorkflowSpec, plan: Record<string, unknown>): Promise<WorkflowOutcome>
  writeBlackboard(key: string, content: string | undefined): Promise<void>
}

export function dynamicAgentTask(prompt: string, options?: DynamicWorkflowAgentOptions): DynamicWorkflowAgentRequest {
  if (!prompt.trim()) throw new Error("dynamic workflow agent prompt must not be empty")
  return { prompt, ...(options ? { options } : {}) }
}

export class DynamicWorkflowContextImpl<TArgs extends Record<string, unknown>> implements DynamicWorkflowContext<TArgs> {
  readonly args: Readonly<TArgs>
  readonly progress: DynamicWorkflowProgress
  private readonly limits: Required<DynamicWorkflowLimits>
  private readonly phaseStack: DynamicWorkflowPhaseProgress[] = []
  private readonly board = new Map<string, { value: unknown; phase?: DynamicWorkflowPhaseProgress }>()
  private nextNode = 0
  private sequence = 0
  private activeSlots = 0
  private readonly waitingSlots: Array<() => void> = []

  readonly blackboard: DynamicWorkflowBlackboard = Object.freeze({
    put: (key: string, value: unknown, options?: { scope?: "workflow" | "phase" }) => this.blackboardPut(key, value, options),
    get: (key: string) => this.board.get(key)?.value,
    delete: (key: string) => this.blackboardDelete(key),
  })

  constructor(private readonly host: DynamicWorkflowHost, runId: string, args: TArgs, limits?: DynamicWorkflowLimits) {
    this.args = Object.freeze({ ...args })
    this.limits = { ...DEFAULT_LIMITS, ...limits }
    this.progress = { runId, agentsStarted: 0, agentsCompleted: 0, activeAgents: 0, phases: [], logs: [] }
  }

  async agent<T = string>(prompt: string, options?: DynamicWorkflowAgentOptions): Promise<DynamicWorkflowAgentResult<T> | null> {
    return (await this.runBatch([dynamicAgentTask(prompt, options)]))[0] as DynamicWorkflowAgentResult<T> | null
  }

  async parallelAgents<T>(items: readonly T[], worker: (item: T, index: number) => DynamicWorkflowAgentRequest): Promise<Array<DynamicWorkflowAgentResult | null>> {
    this.assertBatch(items.length, "parallelAgents")
    return this.runBatch(items.map((item, index) => worker(item, index)))
  }

  async parallel<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]> {
    this.assertBatch(items.length, "parallel")
    return Promise.all(items.map((item, index) => worker(item, index)))
  }

  async pipeline<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]> {
    const out: R[] = []
    for (let index = 0; index < items.length; index++) out.push(await worker(items[index], index))
    return out
  }

  async phase<T>(name: string, body: () => Promise<T> | T): Promise<T> {
    if (!name.trim()) throw new Error("dynamic workflow phase name must not be empty")
    const phase: DynamicWorkflowPhaseProgress = { name, status: "running", agentsStarted: 0, agentsCompleted: 0 }
    this.progress.phases.push(phase)
    this.phaseStack.push(phase)
    this.progress.phase = name
    try {
      const result = await body()
      phase.status = "completed"
      return result
    } catch (error) {
      phase.status = "failed"
      throw error
    } finally {
      this.phaseStack.pop()
      this.progress.phase = this.phaseStack.at(-1)?.name
      for (const [key, entry] of [...this.board]) {
        if (entry.phase === phase) await this.blackboardDelete(key)
      }
    }
  }

  log(message: string, fields?: Record<string, unknown>): void {
    this.progress.logs.push({ message, ...(fields ? { fields } : {}) })
  }

  private assertBatch(size: number, operation: string): void {
    if (size > this.limits.maxBatchSize) {
      throw new DynamicWorkflowLimitError(`${operation} exceeded maxBatchSize (${this.limits.maxBatchSize})`)
    }
  }

  private async blackboardPut(key: string, value: unknown, options: { scope?: "workflow" | "phase" } = {}): Promise<void> {
    if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(key)) {
      throw new Error("dynamic workflow blackboard keys are 1-128 characters of letters, digits, _ . : -")
    }
    const scope = options.scope ?? "workflow"
    if (scope !== "workflow" && scope !== "phase") throw new Error(`unknown blackboard scope ${String(scope)}`)
    const phase = scope === "phase" ? this.phaseStack.at(-1) : undefined
    if (scope === "phase" && !phase) throw new Error("a phase-scoped blackboard entry needs an enclosing phase")
    const content = typeof value === "string" ? value : JSON.stringify(value)
    if (content === undefined) throw new Error("dynamic workflow blackboard values must be serializable")
    this.board.set(key, { value: typeof value === "string" ? value : JSON.parse(content), ...(phase ? { phase } : {}) })
    await this.host.writeBlackboard(key, content)
  }

  private async blackboardDelete(key: string): Promise<void> {
    if (!this.board.delete(key)) return
    await this.host.writeBlackboard(key, undefined)
  }

  private async acquireSlot(): Promise<void> {
    if (this.activeSlots < this.limits.maxConcurrentAgents) {
      this.activeSlots += 1
      return
    }
    await new Promise<void>(resolve => this.waitingSlots.push(resolve))
  }

  private releaseSlot(): void {
    const next = this.waitingSlots.shift()
    if (next) next()
    else this.activeSlots -= 1
  }

  private async runBatch(requests: readonly DynamicWorkflowAgentRequest[]): Promise<Array<DynamicWorkflowAgentResult | null>> {
    if (requests.length === 0) return []
    if (this.progress.agentsStarted + requests.length > this.limits.maxAgentsPerRun) {
      throw new DynamicWorkflowLimitError(`dynamic workflow exceeded maxAgentsPerRun (${this.limits.maxAgentsPerRun})`)
    }
    const sequence = this.sequence++
    const entries = requests.map(request => {
      const options = request.options ?? {}
      const nodeId = options.label?.trim() || `dynamic-agent-${this.nextNode++}`
      const fingerprint = `sha256:${sha256Hex(JSON.stringify({ prompt: request.prompt, options }))}`
      const node: WorkflowNodeSpec = {
        nodeId,
        task: request.prompt,
        role: options.role ?? "implement",
        ...(options.modelHint ? { modelHint: options.modelHint } : {}),
        ...(options.trust ? { trust: options.trust } : {}),
        ...(options.isolation ? { isolation: options.isolation } : {}),
        ...(options.outputSchema ? { outputSchema: options.outputSchema } : {}),
        ...(options.tokenBudget !== undefined ? { tokenBudget: options.tokenBudget } : {}),
        ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
        ...(options.maxWallMs !== undefined ? { maxWallMs: options.maxWallMs } : {}),
      }
      return { nodeId, request, fingerprint, node }
    })
    const phase = this.phaseStack.at(-1)
    this.progress.agentsStarted += entries.length
    this.progress.activeAgents += entries.length
    if (phase) phase.agentsStarted += entries.length
    await this.acquireSlot()
    let outcome: WorkflowOutcome
    try {
      outcome = await this.host.runWorkflow({ nodes: entries.map(entry => entry.node) }, {
        run_id: this.progress.runId,
        sequence,
        nodes: entries.map(entry => ({ node_id: entry.nodeId, depends_on: [], prompt_fingerprint: entry.fingerprint, replay: "executed" })),
      })
    } finally {
      this.releaseSlot()
      this.progress.activeAgents -= entries.length
    }
    if (outcome.rejection) throw new Error(`dynamic workflow submission rejected: ${outcome.rejection.reason}`)
    const byNode = new Map(outcome.nodeOutcomes.map(node => [node.nodeId, node]))
    return entries.map(entry => {
      this.progress.agentsCompleted += 1
      if (phase) phase.agentsCompleted += 1
      const node = byNode.get(entry.nodeId)
      const text = outcome.outputs[entry.nodeId]
      if (text === undefined && (!node || node.status !== "completed")) return null
      let value: unknown = text ?? ""
      if (entry.request.options?.outputSchema) {
        try { value = JSON.parse(text ?? "") } catch { value = null }
      }
      return {
        value,
        text: text ?? "",
        nodeId: entry.nodeId,
        status: node?.status ?? "completed",
        ...(node?.termination ? { termination: node.termination } : {}),
      } as DynamicWorkflowAgentResult
    })
  }
}
