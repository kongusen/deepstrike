import type {
  KernelAgentRole,
  WorkflowNodeSpec,
  WorkflowNodeStatus,
  WorkflowOutcome,
  WorkflowSpec,
} from "../types/agent.js"

/** The sizing hint sent to a workflow author. It is guidance, not a hard agent count. */
export type DynamicWorkflowSizeGuideline = "small" | "medium" | "large" | "unrestricted"

/** Lifecycle states exposed by the host-side dynamic workflow controller. */
export type DynamicWorkflowStatus = "planning" | "running" | "completed" | "failed" | "cancelled"

/**
 * Runtime limits modelled after the public dynamic-workflow contract.
 *
 * These limits are checked before work is submitted to the kernel. The kernel's own quota is
 * still authoritative, so this is a workflow-level guardrail rather than a replacement for
 * `ResourceQuota`.
 */
export interface DynamicWorkflowLimits {
  /** Default 16; maximum accepted value is 256. */
  maxConcurrentAgents?: number
  /** Default 1000 agents for one run. */
  maxAgentsPerRun?: number
  /** Default 4096 items in one parallel/pipeline call. */
  maxItemsPerBatch?: number
}

export const DEFAULT_DYNAMIC_WORKFLOW_LIMITS: Required<DynamicWorkflowLimits> = Object.freeze({
  maxConcurrentAgents: 16,
  maxAgentsPerRun: 1000,
  maxItemsPerBatch: 4096,
})

export interface DynamicWorkflowMeta {
  name: string
  description: string
  phases?: string[]
  sizeGuideline?: DynamicWorkflowSizeGuideline
}

/** A persisted script artifact. Execution of source text is intentionally a later, isolated slice. */
export interface DynamicWorkflowScript {
  meta: DynamicWorkflowMeta
  source: string
}

export interface DynamicWorkflowLogEntry {
  message: string
  fields?: Record<string, unknown>
  timestamp: number
}

export interface DynamicWorkflowPhaseProgress {
  name: string
  status: "running" | "completed" | "failed"
  agentsStarted: number
  agentsCompleted: number
}

export interface DynamicWorkflowProgress {
  runId: string
  status: DynamicWorkflowStatus
  phase?: string
  agentsStarted: number
  agentsCompleted: number
  activeAgents: number
  phases: DynamicWorkflowPhaseProgress[]
  logs: DynamicWorkflowLogEntry[]
}

export interface DynamicWorkflowAgentOptions {
  label?: string
  role?: KernelAgentRole
  modelHint?: string
  isolation?: WorkflowNodeSpec["isolation"]
  outputSchema?: Record<string, unknown>
  tokenBudget?: number
  maxTurns?: number
  maxWallMs?: number
}

/** A declarative agent request that can be admitted as one kernel workflow batch. */
export interface DynamicWorkflowAgentRequest {
  prompt: string
  options?: DynamicWorkflowAgentOptions
}

export interface DynamicWorkflowAgentResult<T = string> {
  value: T
  text: string
  nodeId: string
  status: WorkflowNodeStatus
  termination?: string
}

export interface DynamicWorkflowHost {
  /** Submit a kernel-owned workflow and return its typed host result. */
  runWorkflow(spec: WorkflowSpec): Promise<WorkflowOutcome>
}

export interface DynamicWorkflowRun<T> {
  runId: string
  value: T
  progress: DynamicWorkflowProgress
}

export interface DynamicWorkflowContext<TArgs extends Record<string, unknown> = Record<string, unknown>> {
  readonly args: Readonly<TArgs>
  readonly progress: DynamicWorkflowProgress
  agent<T = string>(prompt: string, options?: DynamicWorkflowAgentOptions): Promise<DynamicWorkflowAgentResult<T> | null>
  parallel<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]>
  parallelAgents<T>(items: readonly T[], worker: (item: T, index: number) => DynamicWorkflowAgentRequest): Promise<Array<DynamicWorkflowAgentResult | null>>
  pipeline<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]>
  phase<T>(name: string, body: () => Promise<T> | T): Promise<T>
  log(message: string, fields?: Record<string, unknown>): void
}

export interface DynamicWorkflowRunOptions<TArgs extends Record<string, unknown> = Record<string, unknown>> {
  runId?: string
  args?: TArgs
  limits?: DynamicWorkflowLimits
  onProgress?: (progress: DynamicWorkflowProgress) => void
}

export function dynamicAgentTask(prompt: string, options?: DynamicWorkflowAgentOptions): DynamicWorkflowAgentRequest {
  if (!prompt.trim()) throw new Error("dynamic workflow agent prompt must not be empty")
  return { prompt, ...(options ? { options: { ...options } } : {}) }
}

export class DynamicWorkflowLimitError extends Error {
  readonly code = "DYNAMIC_WORKFLOW_LIMIT"

  constructor(message: string) {
    super(message)
    this.name = "DynamicWorkflowLimitError"
  }
}

export function resolveDynamicWorkflowLimits(limits?: DynamicWorkflowLimits): Required<DynamicWorkflowLimits> {
  const resolved = { ...DEFAULT_DYNAMIC_WORKFLOW_LIMITS, ...limits }
  if (!Number.isInteger(resolved.maxConcurrentAgents) || resolved.maxConcurrentAgents < 1 || resolved.maxConcurrentAgents > 256) {
    throw new DynamicWorkflowLimitError("maxConcurrentAgents must be an integer from 1 to 256")
  }
  if (!Number.isInteger(resolved.maxAgentsPerRun) || resolved.maxAgentsPerRun < 1) {
    throw new DynamicWorkflowLimitError("maxAgentsPerRun must be a positive integer")
  }
  if (!Number.isInteger(resolved.maxItemsPerBatch) || resolved.maxItemsPerBatch < 1) {
    throw new DynamicWorkflowLimitError("maxItemsPerBatch must be a positive integer")
  }
  return resolved
}

/**
 * Host-side controller for the dynamic workflow vocabulary.
 *
 * `agent()` delegates to the existing `RuntimeRunner.runWorkflow()` entry point, so every child
 * still crosses the kernel syscall gate. `parallel()` and `pipeline()` provide the dynamic script
 * semantics now; a later controller can replace the host callback with one long-lived kernel DAG
 * without changing the public vocabulary.
 */
export class DynamicWorkflowExecutor<TArgs extends Record<string, unknown> = Record<string, unknown>> {
  private readonly host: DynamicWorkflowHost
  private readonly limits: Required<DynamicWorkflowLimits>
  private readonly options: DynamicWorkflowRunOptions<TArgs>

  constructor(host: DynamicWorkflowHost, options: DynamicWorkflowRunOptions<TArgs> = {}) {
    this.host = host
    this.limits = resolveDynamicWorkflowLimits(options.limits)
    this.options = options
  }

  async run<T>(program: (context: DynamicWorkflowContext<TArgs>) => Promise<T> | T): Promise<DynamicWorkflowRun<T>> {
    const runId = this.options.runId ?? `dw-${crypto.randomUUID()}`
    const progress: DynamicWorkflowProgress = {
      runId,
      status: "planning",
      agentsStarted: 0,
      agentsCompleted: 0,
      activeAgents: 0,
      phases: [],
      logs: [],
    }
    const context = new DynamicWorkflowContextImpl<TArgs>(
      this.host,
      this.limits,
      progress,
      this.options.args ?? {} as TArgs,
      this.options.onProgress,
    )
    context.setStatus("running")
    try {
      const value = await program(context)
      context.setStatus("completed")
      return { runId, value, progress }
    } catch (error) {
      context.setStatus("failed")
      throw error
    }
  }
}

class DynamicWorkflowContextImpl<TArgs extends Record<string, unknown>> implements DynamicWorkflowContext<TArgs> {
  readonly args: Readonly<TArgs>
  private agentCount = 0
  private nextNode = 0
  private readonly phaseStack: DynamicWorkflowPhaseProgress[] = []
  private activeAgentSlots = 0
  private readonly waitingAgentSlots: Array<() => void> = []

  constructor(
    private readonly host: DynamicWorkflowHost,
    private readonly limits: Required<DynamicWorkflowLimits>,
    readonly progress: DynamicWorkflowProgress,
    args: TArgs,
    private readonly onProgress?: (progress: DynamicWorkflowProgress) => void,
  ) {
    this.args = snapshotArgs(args)
  }

  setStatus(status: DynamicWorkflowStatus): void {
    this.progress.status = status
    this.emit()
  }

  agent<T = string>(prompt: string, options: DynamicWorkflowAgentOptions = {}): Promise<DynamicWorkflowAgentResult<T> | null> {
    return this.runAgent<T>(prompt, options)
  }

  async parallelAgents<T>(
    items: readonly T[],
    worker: (item: T, index: number) => DynamicWorkflowAgentRequest,
  ): Promise<Array<DynamicWorkflowAgentResult | null>> {
    this.assertBatchSize(items.length, "parallelAgents")
    const requests = items.map(worker)
    const chunks: DynamicWorkflowAgentRequest[][] = []
    for (let index = 0; index < requests.length; index += this.limits.maxConcurrentAgents) {
      chunks.push(requests.slice(index, index + this.limits.maxConcurrentAgents))
    }
    const results: Array<DynamicWorkflowAgentResult | null> = []
    for (const chunk of chunks) results.push(...await this.runAgentBatch(chunk))
    return results
  }

  async parallel<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]> {
    this.assertBatchSize(items.length, "parallel")
    return this.mapBounded(items, worker)
  }

  async pipeline<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]> {
    this.assertBatchSize(items.length, "pipeline")
    const results: R[] = []
    for (let index = 0; index < items.length; index += 1) {
      results.push(await worker(items[index], index))
    }
    return results
  }

  async phase<T>(name: string, body: () => Promise<T> | T): Promise<T> {
    if (!name.trim()) throw new Error("dynamic workflow phase name must not be empty")
    const phase: DynamicWorkflowPhaseProgress = { name, status: "running", agentsStarted: 0, agentsCompleted: 0 }
    this.progress.phases.push(phase)
    this.phaseStack.push(phase)
    this.progress.phase = name
    this.emit()
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
      this.emit()
    }
  }

  log(message: string, fields?: Record<string, unknown>): void {
    this.progress.logs.push({ message, ...(fields ? { fields: { ...fields } } : {}), timestamp: Date.now() })
    this.emit()
  }

  private async mapBounded<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]> {
    const results = new Array<R>(items.length)
    let cursor = 0
    const runWorker = async (): Promise<void> => {
      for (;;) {
        const index = cursor++
        if (index >= items.length) return
        results[index] = await worker(items[index], index)
      }
    }
    await Promise.all(Array.from({ length: Math.min(this.limits.maxConcurrentAgents, items.length) }, () => runWorker()))
    return results
  }

  private async runAgent<T>(prompt: string, options: DynamicWorkflowAgentOptions): Promise<DynamicWorkflowAgentResult<T> | null> {
    if (!prompt.trim()) throw new Error("dynamic workflow agent prompt must not be empty")
    if (this.agentCount >= this.limits.maxAgentsPerRun) {
      throw new DynamicWorkflowLimitError(
        `dynamic workflow exceeded maxAgentsPerRun (${this.limits.maxAgentsPerRun})`,
      )
    }
    const nodeId = options.label?.trim() || `dynamic-agent-${this.nextNode++}`
    this.agentCount += 1
    await this.acquireAgentSlot()
    this.progress.agentsStarted = this.agentCount
    this.progress.activeAgents += 1
    const currentPhase = this.phaseStack.at(-1)
    if (currentPhase) {
      currentPhase.agentsStarted += 1
    }
    this.emit()
    try {
      const spec: WorkflowSpec = {
        nodes: [{
          nodeId,
          task: prompt,
          role: options.role ?? "implement",
          ...(options.modelHint ? { modelHint: options.modelHint } : {}),
          ...(options.isolation ? { isolation: options.isolation } : {}),
          ...(options.outputSchema ? { outputSchema: options.outputSchema } : {}),
          ...(options.tokenBudget !== undefined ? { tokenBudget: options.tokenBudget } : {}),
          ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
          ...(options.maxWallMs !== undefined ? { maxWallMs: options.maxWallMs } : {}),
        }],
      }
      const outcome = await this.host.runWorkflow(spec)
      if (outcome.rejection) return null
      const node = outcome.nodeOutcomes.find(candidate => candidate.nodeId === nodeId) ?? outcome.nodeOutcomes[0]
      const text = node?.output?.content ?? Object.values(outcome.outputs)[0] ?? ""
      const value = options.outputSchema ? parseStructuredValue<T>(text) : (text as T)
      this.progress.agentsCompleted += 1
      if (currentPhase) currentPhase.agentsCompleted += 1
      return {
        value,
        text,
        nodeId: node?.nodeId ?? nodeId,
        status: node?.status ?? "completed_partial",
        ...(node?.termination ? { termination: node.termination } : {}),
      }
    } finally {
      this.progress.activeAgents -= 1
      this.releaseAgentSlot()
      this.emit()
    }
  }

  private async runAgentBatch(requests: readonly DynamicWorkflowAgentRequest[]): Promise<Array<DynamicWorkflowAgentResult | null>> {
    if (requests.length === 0) return []
    if (this.agentCount + requests.length > this.limits.maxAgentsPerRun) {
      throw new DynamicWorkflowLimitError(
        `dynamic workflow exceeded maxAgentsPerRun (${this.limits.maxAgentsPerRun})`,
      )
    }
    const nodes = requests.map((request, index) => {
      const options = request.options ?? {}
      const nodeId = options.label?.trim() || `dynamic-agent-${this.nextNode++}`
      return {
        nodeId,
        request,
        options,
        node: {
          nodeId,
          task: request.prompt,
          role: options.role ?? "implement",
          ...(options.modelHint ? { modelHint: options.modelHint } : {}),
          ...(options.isolation ? { isolation: options.isolation } : {}),
          ...(options.outputSchema ? { outputSchema: options.outputSchema } : {}),
          ...(options.tokenBudget !== undefined ? { tokenBudget: options.tokenBudget } : {}),
          ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
          ...(options.maxWallMs !== undefined ? { maxWallMs: options.maxWallMs } : {}),
        } satisfies WorkflowNodeSpec,
      }
    })
    this.agentCount += nodes.length
    this.progress.agentsStarted = this.agentCount
    this.progress.activeAgents += nodes.length
    const currentPhase = this.phaseStack.at(-1)
    if (currentPhase) currentPhase.agentsStarted += nodes.length
    this.emit()
    try {
      const outcome = await this.host.runWorkflow({ nodes: nodes.map(entry => entry.node) })
      if (outcome.rejection) return nodes.map(() => null)
      return nodes.map(entry => {
        const node = outcome.nodeOutcomes.find(candidate => candidate.nodeId === entry.nodeId)
        const text = node?.output?.content ?? outcome.outputs[entry.nodeId] ?? ""
        this.progress.agentsCompleted += 1
        if (currentPhase) currentPhase.agentsCompleted += 1
        return {
          value: entry.options.outputSchema ? parseStructuredValue(text) : text,
          text,
          nodeId: node?.nodeId ?? entry.nodeId,
          status: node?.status ?? "completed_partial",
          ...(node?.termination ? { termination: node.termination } : {}),
        }
      })
    } finally {
      this.progress.activeAgents -= nodes.length
      this.emit()
    }
  }

  private async acquireAgentSlot(): Promise<void> {
    if (this.activeAgentSlots < this.limits.maxConcurrentAgents) {
      this.activeAgentSlots += 1
      return
    }
    await new Promise<void>(resolve => this.waitingAgentSlots.push(resolve))
    this.activeAgentSlots += 1
  }

  private releaseAgentSlot(): void {
    this.activeAgentSlots -= 1
    this.waitingAgentSlots.shift()?.()
  }

  private assertBatchSize(size: number, operation: string): void {
    if (size > this.limits.maxItemsPerBatch) {
      throw new DynamicWorkflowLimitError(
        `${operation} accepts at most ${this.limits.maxItemsPerBatch} items (received ${size})`,
      )
    }
  }

  private emit(): void {
    this.onProgress?.(this.progress)
  }
}

function parseStructuredValue<T>(text: string): T {
  try {
    return JSON.parse(text) as T
  } catch {
    return text as T
  }
}

function snapshotArgs<T extends Record<string, unknown>>(args: T): Readonly<T> {
  const clone = structuredClone(args)
  return deepFreeze(clone)
}

function deepFreeze<T>(value: T): T {
  if (value === null || typeof value !== "object" || Object.isFrozen(value)) return value
  Object.freeze(value)
  for (const child of Object.values(value as Record<string, unknown>)) deepFreeze(child)
  return value
}
