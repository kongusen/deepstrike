import type {
  KernelAgentRole,
  WorkflowNodeSpec,
  WorkflowNodeStatus,
  WorkflowOutcome,
  WorkflowSpec,
} from "../types/agent.js"
import {
  fingerprintDynamicWorkflowRun,
  fingerprintDynamicWorkflowInvocation,
  type DynamicWorkflowInvocationRecord,
  type DynamicWorkflowReplayRun,
  type DynamicWorkflowReplayStore,
} from "./dynamic-replay.js"
import { createHash } from "node:crypto"

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

export type DynamicWorkflowLifecycleEvent =
  | { kind: "run_started"; runId: string }
  | { kind: "approval_requested"; runId: string }
  | { kind: "approval_resolved"; runId: string; approved: boolean; reason?: string }
  | { kind: "phase_started"; runId: string; name: string }
  | { kind: "phase_completed"; runId: string; name: string }
  | { kind: "phase_failed"; runId: string; name: string; error: string }
  | { kind: "agent_started"; runId: string; nodeId: string; promptFingerprint: string }
  | { kind: "agent_reused"; runId: string; nodeId: string; promptFingerprint: string }
  | { kind: "agent_completed"; runId: string; nodeId: string; status: WorkflowNodeStatus; termination?: string }
  | { kind: "log"; runId: string; message: string; fields?: Record<string, unknown> }
  | { kind: "run_completed"; runId: string }
  | { kind: "run_failed"; runId: string; error: string }
  | { kind: "run_cancelled"; runId: string; reason: string }

export interface DynamicWorkflowApprovalRequest<TArgs extends Record<string, unknown> = Record<string, unknown>> {
  runId: string
  args: Readonly<TArgs>
  limits: Required<DynamicWorkflowLimits>
}

/** A persisted script artifact. Execution of source text is intentionally a later, isolated slice. */
export interface DynamicWorkflowScript {
  meta: DynamicWorkflowMeta
  source: string
}

export interface DynamicWorkflowArtifact {
  name: string
  digest: string
  script: DynamicWorkflowScript
  origin?: string
}

export function fingerprintDynamicWorkflowScript(script: DynamicWorkflowScript): string {
  return createHash("sha256").update(JSON.stringify(script)).digest("hex")
}

export function createDynamicWorkflowArtifact(script: DynamicWorkflowScript, origin?: string): DynamicWorkflowArtifact {
  return { name: script.meta.name, digest: fingerprintDynamicWorkflowScript(script), script, ...(origin ? { origin } : {}) }
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
  agentsReused: number
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
  events: readonly DynamicWorkflowLifecycleEvent[]
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
  replayStore?: DynamicWorkflowReplayStore
  approval?: (request: DynamicWorkflowApprovalRequest<TArgs>) => boolean | Promise<boolean> | { approved: boolean; reason?: string } | Promise<{ approved: boolean; reason?: string }>
  onLifecycleEvent?: (event: DynamicWorkflowLifecycleEvent) => void
  artifactDigest?: string
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

export class DynamicWorkflowApprovalError extends Error {
  readonly code = "DYNAMIC_WORKFLOW_APPROVAL_REQUIRED"

  constructor(message = "dynamic workflow execution was not approved") {
    super(message)
    this.name = "DynamicWorkflowApprovalError"
  }
}

export class DynamicWorkflowReplayMismatchError extends Error {
  readonly code = "DYNAMIC_WORKFLOW_REPLAY_MISMATCH"

  constructor(message = "dynamic workflow replay inputs do not match the stored run") {
    super(message)
    this.name = "DynamicWorkflowReplayMismatchError"
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
    const args = structuredClone(this.options.args ?? {} as TArgs) as Record<string, unknown>
    const inputFingerprint = fingerprintDynamicWorkflowRun({ artifactDigest: this.options.artifactDigest, args, limits: this.limits })
    const existing = await this.options.replayStore?.loadRun?.(runId)
    if (existing?.inputFingerprint && existing.inputFingerprint !== inputFingerprint) {
      throw new DynamicWorkflowReplayMismatchError(`dynamic workflow replay inputs changed for run "${runId}"`)
    }
    const replayRun: DynamicWorkflowReplayRun = {
      version: 2,
      runId,
      inputFingerprint,
      ...(this.options.artifactDigest ? { artifactDigest: this.options.artifactDigest } : {}),
      argsFingerprint: fingerprintDynamicWorkflowRun({ args, limits: this.limits }),
      limits: this.limits,
      status: "planning",
      events: existing?.events ? [...existing.events] : [],
      records: existing?.records ?? [],
    }
    if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
    const events: DynamicWorkflowLifecycleEvent[] = []
    const emitLifecycle = (event: DynamicWorkflowLifecycleEvent): void => {
      events.push(event)
      replayRun.events.push(event)
      this.options.onLifecycleEvent?.(event)
      void this.options.replayStore?.appendEvent?.(runId, event)
    }
    emitLifecycle({ kind: "run_started", runId })
    const progress: DynamicWorkflowProgress = {
      runId,
      status: "planning",
      agentsStarted: 0,
      agentsCompleted: 0,
      activeAgents: 0,
      agentsReused: 0,
      phases: [],
      logs: [],
    }
    const context = new DynamicWorkflowContextImpl<TArgs>(
      this.host,
      this.limits,
      progress,
      this.options.args ?? {} as TArgs,
      runId,
      this.options.replayStore,
      this.options.onProgress,
      emitLifecycle,
    )
    emitLifecycle({ kind: "approval_requested", runId })
    const approval = this.options.approval
      ? await this.options.approval({ runId, args: context.args, limits: this.limits })
      : true
    const approved = typeof approval === "boolean" ? approval : approval.approved
    const reason = typeof approval === "boolean" ? undefined : approval.reason
    emitLifecycle({ kind: "approval_resolved", runId, approved, ...(reason ? { reason } : {}) })
    if (!approved) {
      context.setStatus("cancelled")
      emitLifecycle({ kind: "run_cancelled", runId, reason: reason ?? "approval denied" })
      replayRun.status = "cancelled"
      replayRun.events = [...replayRun.events]
      if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
      throw new DynamicWorkflowApprovalError(reason ?? undefined)
    }
    context.setStatus("running")
    replayRun.status = "running"
    try {
      const value = await program(context)
      context.setStatus("completed")
      emitLifecycle({ kind: "run_completed", runId })
      replayRun.status = "completed"
      replayRun.events = [...replayRun.events]
      replayRun.records = (await this.options.replayStore?.loadRun?.(runId))?.records ?? replayRun.records
      if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
      return { runId, value, progress, events }
    } catch (error) {
      context.setStatus("failed")
      emitLifecycle({ kind: "run_failed", runId, error: error instanceof Error ? error.message : String(error) })
      replayRun.status = "failed"
      replayRun.events = [...events]
      replayRun.records = (await this.options.replayStore?.loadRun?.(runId))?.records ?? replayRun.records
      if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
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
    private readonly runId: string,
    private readonly replayStore: DynamicWorkflowReplayStore | undefined,
    private readonly onProgress?: (progress: DynamicWorkflowProgress) => void,
    private readonly emitLifecycle?: (event: DynamicWorkflowLifecycleEvent) => void,
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
    this.emitLifecycle?.({ kind: "phase_started", runId: this.runId, name })
    this.emit()
    try {
      const result = await body()
      phase.status = "completed"
      this.emitLifecycle?.({ kind: "phase_completed", runId: this.runId, name })
      return result
    } catch (error) {
      phase.status = "failed"
      this.emitLifecycle?.({ kind: "phase_failed", runId: this.runId, name, error: error instanceof Error ? error.message : String(error) })
      throw error
    } finally {
      this.phaseStack.pop()
      this.progress.phase = this.phaseStack.at(-1)?.name
      this.emit()
    }
  }

  log(message: string, fields?: Record<string, unknown>): void {
    this.progress.logs.push({ message, ...(fields ? { fields: { ...fields } } : {}), timestamp: Date.now() })
    this.emitLifecycle?.({ kind: "log", runId: this.runId, message, ...(fields ? { fields: { ...fields } } : {}) })
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
    const nodeId = options.label?.trim() || `dynamic-agent-${this.nextNode++}`
    const promptFingerprint = fingerprintDynamicWorkflowInvocation(prompt, options)
    const cached = await this.replayStore?.find(this.runId, nodeId, promptFingerprint)
    if (cached) {
      this.emitLifecycle?.({ kind: "agent_reused", runId: this.runId, nodeId, promptFingerprint })
      this.progress.agentsReused += 1
      this.progress.agentsCompleted += 1
      const currentPhase = this.phaseStack.at(-1)
      if (currentPhase) {
        currentPhase.agentsStarted += 1
        currentPhase.agentsCompleted += 1
      }
      this.emit()
      return {
        value: options.outputSchema ? parseStructuredValue<T>(cached.text) : cached.text as T,
        text: cached.text,
        nodeId: cached.nodeId,
        status: replayStatus(cached.status),
        ...(cached.termination ? { termination: cached.termination } : {}),
      }
    }
    if (this.agentCount >= this.limits.maxAgentsPerRun) {
      throw new DynamicWorkflowLimitError(
        `dynamic workflow exceeded maxAgentsPerRun (${this.limits.maxAgentsPerRun})`,
      )
    }
    this.agentCount += 1
    this.emitLifecycle?.({ kind: "agent_started", runId: this.runId, nodeId, promptFingerprint })
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
      let outcome: WorkflowOutcome
      try {
        outcome = await this.host.runWorkflow(spec)
      } catch (error) {
        await this.replayStore?.save(this.runId, {
          nodeId,
          promptFingerprint,
          text: "",
          status: "failed",
          termination: "host_error",
          error: error instanceof Error ? error.message : String(error),
        })
        this.emitLifecycle?.({ kind: "agent_completed", runId: this.runId, nodeId, status: "failed", termination: "host_error" })
        throw error
      }
      if (outcome.rejection) {
        await this.replayStore?.save(this.runId, {
          nodeId,
          promptFingerprint,
          text: "",
          status: "cancelled",
          termination: "workflow_rejected",
        })
        this.emitLifecycle?.({ kind: "agent_completed", runId: this.runId, nodeId, status: "failed", termination: "workflow_rejected" })
        return null
      }
      const node = outcome.nodeOutcomes.find(candidate => candidate.nodeId === nodeId) ?? outcome.nodeOutcomes[0]
      const text = node?.output?.content ?? Object.values(outcome.outputs)[0] ?? ""
      const value = options.outputSchema ? parseStructuredValue<T>(text) : (text as T)
      const status = node?.status ?? "failed"
      await this.replayStore?.save(this.runId, {
        nodeId,
        promptFingerprint,
        text,
        status,
        ...(node?.termination ? { termination: node.termination } : { termination: "missing_node_outcome" }),
        ...(node ? {} : { error: "workflow returned no matching node outcome" }),
      })
      this.emitLifecycle?.({ kind: "agent_completed", runId: this.runId, nodeId, status, ...(node?.termination ? { termination: node.termination } : { termination: "missing_node_outcome" }) })
      this.progress.agentsCompleted += 1
      if (currentPhase) currentPhase.agentsCompleted += 1
      return {
        value,
        text,
        nodeId: node?.nodeId ?? nodeId,
        status,
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
    const cached = await Promise.all(nodes.map(async entry => {
      const promptFingerprint = fingerprintDynamicWorkflowInvocation(entry.request.prompt, entry.options)
      return {
        ...entry,
        promptFingerprint,
        record: await this.replayStore?.find(this.runId, entry.nodeId, promptFingerprint),
      }
    }))
    const misses = cached.filter(entry => !entry.record)
    if (this.agentCount + misses.length > this.limits.maxAgentsPerRun) {
      throw new DynamicWorkflowLimitError(
        `dynamic workflow exceeded maxAgentsPerRun (${this.limits.maxAgentsPerRun})`,
      )
    }
    const currentPhase = this.phaseStack.at(-1)
    const replayed = cached.filter(entry => entry.record)
    if (replayed.length > 0) {
      this.progress.agentsReused += replayed.length
      this.progress.agentsCompleted += replayed.length
      if (currentPhase) {
        currentPhase.agentsStarted += replayed.length
        currentPhase.agentsCompleted += replayed.length
      }
      this.emit()
      for (const entry of replayed) this.emitLifecycle?.({ kind: "agent_reused", runId: this.runId, nodeId: entry.nodeId, promptFingerprint: entry.promptFingerprint })
    }
    if (misses.length === 0) {
      return cached.map(entry => entry.record ? this.replayResult(entry.options, entry.record) : null)
    }
    this.agentCount += misses.length
    this.progress.agentsStarted = this.agentCount
    this.progress.activeAgents += misses.length
    if (currentPhase) currentPhase.agentsStarted += misses.length
    this.emit()
    try {
      let outcome: WorkflowOutcome
      try {
        outcome = await this.host.runWorkflow({ nodes: misses.map(entry => entry.node) })
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error)
        await Promise.all(misses.map(entry => this.replayStore?.save(this.runId, {
          nodeId: entry.nodeId,
          promptFingerprint: entry.promptFingerprint,
          text: "",
          status: "failed",
          termination: "host_error",
          error: message,
        })))
        for (const entry of misses) this.emitLifecycle?.({ kind: "agent_completed", runId: this.runId, nodeId: entry.nodeId, status: "failed", termination: "host_error" })
        throw error
      }
      if (outcome.rejection) {
        await Promise.all(misses.map(entry => this.replayStore?.save(this.runId, {
          nodeId: entry.nodeId,
          promptFingerprint: entry.promptFingerprint,
          text: "",
          status: "cancelled",
          termination: "workflow_rejected",
        })))
        return cached.map(entry => entry.record ? this.replayResult(entry.options, entry.record) : null)
      }
      const results = new Map<string, DynamicWorkflowAgentResult | null>()
      for (const entry of misses) {
        const node = outcome.nodeOutcomes.find(candidate => candidate.nodeId === entry.nodeId)
        const text = node?.output?.content ?? outcome.outputs[entry.nodeId] ?? ""
        const status = node?.status ?? "failed"
        const result: DynamicWorkflowAgentResult = {
          value: entry.options.outputSchema ? parseStructuredValue(text) : text,
          text,
          nodeId: node?.nodeId ?? entry.nodeId,
          status,
          ...(node?.termination ? { termination: node.termination } : {}),
        }
        results.set(entry.nodeId, result)
        await this.replayStore?.save(this.runId, {
          nodeId: entry.nodeId,
          promptFingerprint: entry.promptFingerprint,
          text,
          status,
          ...(node?.termination ? { termination: node.termination } : { termination: "missing_node_outcome" }),
          ...(node ? {} : { error: "workflow returned no matching node outcome" }),
        })
        this.emitLifecycle?.({ kind: "agent_completed", runId: this.runId, nodeId: entry.nodeId, status, ...(node?.termination ? { termination: node.termination } : { termination: "missing_node_outcome" }) })
        this.progress.agentsCompleted += 1
        if (currentPhase) currentPhase.agentsCompleted += 1
      }
      return cached.map(entry => entry.record ? this.replayResult(entry.options, entry.record) : results.get(entry.nodeId) ?? null)
    } finally {
      this.progress.activeAgents -= misses.length
      this.emit()
    }
  }

  private replayResult(options: DynamicWorkflowAgentOptions, record: {
    nodeId: string
    text: string
    status: DynamicWorkflowInvocationRecord["status"]
    termination?: string
  }): DynamicWorkflowAgentResult {
    return {
      value: options.outputSchema ? parseStructuredValue(record.text) : record.text,
      text: record.text,
      nodeId: record.nodeId,
      status: replayStatus(record.status),
      ...(record.termination ? { termination: record.termination } : {}),
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

function replayStatus(status: DynamicWorkflowInvocationRecord["status"]): WorkflowNodeStatus {
  if (status === "completed" || status === "completed_partial" || status === "failed" || status === "skipped_upstream_failed") return status
  return "failed"
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
