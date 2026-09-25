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
export type DynamicWorkflowStatus = "planning" | "running" | "paused" | "completed" | "failed" | "cancelled"

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
  | { kind: "pause_requested"; runId: string; reason: string }
  | { kind: "paused"; runId: string; reason: string }
  | { kind: "resumed"; runId: string }
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

/** Immutable artifact identity persisted with a replay run; source bytes remain in the artifact store. */
export interface DynamicWorkflowArtifactSnapshot {
  name: string
  digest: string
  meta: DynamicWorkflowMeta
}

export interface DynamicWorkflowVmOptions {
  /** Maximum source bytes accepted from an artifact. */
  maxSourceBytes?: number
  /** Maximum synchronous VM execution time for one script turn. */
  timeoutMs?: number
  /** Maximum wall time for the complete script, including host workflow submissions. */
  maxExecutionMs?: number
}

/** Trust boundary for dynamic script execution. The in-process VM is only for trusted scripts. */
export type DynamicWorkflowTrust = "trusted" | "untrusted"

export type DynamicWorkflowProgram<TArgs extends Record<string, unknown> = Record<string, unknown>, T = unknown> =
  | ((context: DynamicWorkflowContext<TArgs>) => Promise<T> | T)
  | DynamicWorkflowScript
  | DynamicWorkflowArtifact

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
  /** Host-side public Agent target resolved at the workflow spawn boundary. */
  agent?: string
  role?: KernelAgentRole
  modelHint?: string
  trust?: WorkflowNodeSpec["trust"]
  toolAccess?: WorkflowNodeSpec["toolAccess"]
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

/** A kernel-facing description of one dynamic submission. The script host creates it; the kernel
 * records it together with the admitted DAG append so replay can correlate plan and execution. */
export interface DynamicWorkflowPlan {
  runId: string
  sequence: number
  nodes: ReadonlyArray<{
    nodeId: string
    dependsOn: readonly string[]
    promptFingerprint: string
    replay: "executed"
  }>
}

/** Canonical snake_case wire shape recorded by the kernel for a dynamic plan. */
export interface KernelDynamicWorkflowPlan {
  run_id: string
  sequence: number
  nodes: Array<{
    node_id: string
    depends_on: string[]
    prompt_fingerprint: string
    replay: "executed"
  }>
}

/** Lower a host dynamic plan without carrying script or result payloads into the kernel. */
export function dynamicWorkflowPlanToKernel(plan: DynamicWorkflowPlan): KernelDynamicWorkflowPlan {
  return {
    run_id: plan.runId,
    sequence: plan.sequence,
    nodes: plan.nodes.map(node => ({
      node_id: node.nodeId,
      depends_on: [...node.dependsOn],
      prompt_fingerprint: node.promptFingerprint,
      replay: node.replay,
    })),
  }
}

/** A replay fact is deliberately smaller than the host result. The host keeps result bytes, while
 * the kernel owns the durable identity/status/digest fact used to audit replay decisions. */
export interface DynamicWorkflowReplayFact {
  runId: string
  sequence: number
  nodeId: string
  promptFingerprint: string
  status: WorkflowNodeStatus | "cancelled"
  replay: "executed" | "reused"
  resultDigest: string
  termination?: string
}

/** Canonical snake_case wire shape for a dynamic invocation replay fact. */
export interface KernelDynamicWorkflowReplayFact {
  run_id: string
  sequence: number
  node_id: string
  prompt_fingerprint: string
  status: WorkflowNodeStatus | "cancelled"
  replay: "executed" | "reused"
  result_digest: string
  termination?: string
}

/** Lower a host replay fact into the kernel's compact audit record. */
export function dynamicWorkflowReplayFactToKernel(fact: DynamicWorkflowReplayFact): KernelDynamicWorkflowReplayFact {
  return {
    run_id: fact.runId,
    sequence: fact.sequence,
    node_id: fact.nodeId,
    prompt_fingerprint: fact.promptFingerprint,
    status: fact.status,
    replay: fact.replay,
    result_digest: fact.resultDigest,
    ...(fact.termination ? { termination: fact.termination } : {}),
  }
}

export interface DynamicWorkflowHost {
  /** Submit a kernel-owned workflow and return its typed host result. */
  runWorkflow(spec: WorkflowSpec, plan?: DynamicWorkflowPlan): Promise<WorkflowOutcome>
  /** Persist a replay decision as a kernel-owned fact when the host has one. */
  recordReplayFact?(fact: DynamicWorkflowReplayFact): Promise<void>
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
  /** Defaults to `trusted`; untrusted scripts require an OS sandbox executor. */
  trust?: DynamicWorkflowTrust
  limits?: DynamicWorkflowLimits
  onProgress?: (progress: DynamicWorkflowProgress) => void
  replayStore?: DynamicWorkflowReplayStore
  approval?: (request: DynamicWorkflowApprovalRequest<TArgs>) => boolean | Promise<boolean> | { approved: boolean; reason?: string } | Promise<{ approved: boolean; reason?: string }>
  onLifecycleEvent?: (event: DynamicWorkflowLifecycleEvent) => void
  signal?: AbortSignal
  control?: DynamicWorkflowControl
  artifactDigest?: string
  artifactSnapshot?: DynamicWorkflowArtifactSnapshot
  vmOptions?: DynamicWorkflowVmOptions
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

export class DynamicWorkflowCancellationError extends Error {
  readonly code = "DYNAMIC_WORKFLOW_CANCELLED"

  constructor(message = "dynamic workflow execution was cancelled") {
    super(message)
    this.name = "DynamicWorkflowCancellationError"
  }
}

export class DynamicWorkflowNothingToResumeError extends Error {
  readonly code = "DYNAMIC_WORKFLOW_NOTHING_TO_RESUME"

  constructor(message = "dynamic workflow saved result is missing; refusing to relaunch") {
    super(message)
    this.name = "DynamicWorkflowNothingToResumeError"
  }
}

export class DynamicWorkflowControl {
  private paused = false
  private readonly waiters: Array<() => void> = []
  private runId?: string
  private emit?: (event: DynamicWorkflowLifecycleEvent) => void
  private setStatus?: (status: DynamicWorkflowStatus) => void
  private closed = false

  attach(runId: string, emit: (event: DynamicWorkflowLifecycleEvent) => void, setStatus?: (status: DynamicWorkflowStatus) => void): void {
    this.closed = false
    this.runId = runId
    this.emit = emit
    this.setStatus = setStatus
  }

  pause(reason = "paused by caller"): void {
    if (this.closed || this.paused) return
    this.paused = true
    this.setStatus?.("paused")
    if (this.runId && this.emit) {
      this.emit({ kind: "pause_requested", runId: this.runId, reason })
      this.emit({ kind: "paused", runId: this.runId, reason })
    }
  }

  resume(): void {
    if (this.closed || !this.paused) return
    this.paused = false
    this.setStatus?.("running")
    if (this.runId && this.emit) this.emit({ kind: "resumed", runId: this.runId })
    for (const resolve of this.waiters.splice(0)) resolve()
  }

  get isPaused(): boolean { return this.paused }

  close(): void {
    this.closed = true
    this.paused = false
    for (const resolve of this.waiters.splice(0)) resolve()
  }

  async waitIfPaused(): Promise<void> {
    if (!this.paused) return
    await new Promise<void>(resolve => this.waiters.push(resolve))
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
 * Internal host-side controller for the dynamic workflow vocabulary.
 *
 * `agent()` delegates to the existing `RuntimeRunner.runWorkflow()` entry point, so every child
 * still crosses the kernel syscall gate. `parallel()` and `pipeline()` provide the dynamic script
 * semantics now; a later controller can replace the host callback with one long-lived kernel DAG
 * without changing the public vocabulary.
 */
/** @internal Use `RuntimeRunner.runDynamicWorkflow()` as the public execution entrypoint. */
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
    const existingArtifactDigest = existing?.artifact?.digest
    const requestedArtifactDigest = this.options.artifactSnapshot?.digest ?? this.options.artifactDigest
    if (existingArtifactDigest !== undefined && existingArtifactDigest !== requestedArtifactDigest) {
      throw new DynamicWorkflowReplayMismatchError(`dynamic workflow artifact changed for run "${runId}"`)
    }
    if (existing?.status === "completed" && existing.records.length === 0 && this.options.artifactSnapshot) {
      throw new DynamicWorkflowNothingToResumeError(`dynamic workflow run "${runId}" has no saved invocation results`)
    }
    const replayRun: DynamicWorkflowReplayRun = {
      version: 2,
      runId,
      inputFingerprint,
      ...(this.options.artifactDigest ? { artifactDigest: this.options.artifactDigest } : {}),
      ...(this.options.artifactSnapshot ? { artifact: structuredClone(this.options.artifactSnapshot) } : {}),
      argsFingerprint: fingerprintDynamicWorkflowRun({ args, limits: this.limits }),
      limits: this.limits,
      status: "planning",
      events: existing?.events ? [...existing.events] : [],
      records: existing?.records ?? [],
    }
    if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
    const events: DynamicWorkflowLifecycleEvent[] = []
    const eventWrites: Promise<void>[] = []
    const emitLifecycle = (event: DynamicWorkflowLifecycleEvent): void => {
      events.push(event)
      replayRun.events.push(event)
      this.options.onLifecycleEvent?.(event)
      const write = this.options.replayStore?.appendEvent?.(runId, event)
      if (write) eventWrites.push(write)
    }
    const flushEvents = async (): Promise<void> => {
      if (eventWrites.length === 0) return
      await Promise.all(eventWrites.splice(0))
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
    const control = this.options.control ?? new DynamicWorkflowControl()
    const context = new DynamicWorkflowContextImpl<TArgs>(
      this.host,
      this.limits,
      progress,
      this.options.args ?? {} as TArgs,
      runId,
      existing !== undefined && existing.status !== "planning",
      existing?.status === "completed",
      new Set(existing?.records.map(record => record.nodeId) ?? []),
      control,
      this.options.replayStore,
      this.options.onProgress,
      emitLifecycle,
    )
    control.attach(runId, emitLifecycle, status => context.setStatus(status))
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
      await flushEvents()
      replayRun.status = "cancelled"
      replayRun.events = [...replayRun.events]
      if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
      control.close()
      throw new DynamicWorkflowApprovalError(reason ?? undefined)
    }
    context.setStatus("running")
    replayRun.status = "running"
    try {
      const value = await raceDynamicWorkflowCancellation(Promise.resolve(program(context)), this.options.signal)
      context.setStatus("completed")
      emitLifecycle({ kind: "run_completed", runId })
      await flushEvents()
      replayRun.status = "completed"
      replayRun.events = [...replayRun.events]
      replayRun.records = (await this.options.replayStore?.loadRun?.(runId))?.records ?? replayRun.records
      if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
      control.close()
      return { runId, value, progress, events }
    } catch (error) {
      if (error instanceof DynamicWorkflowCancellationError) {
        context.setStatus("cancelled")
        emitLifecycle({ kind: "run_cancelled", runId, reason: error.message })
        await flushEvents()
        replayRun.status = "cancelled"
        replayRun.events = [...replayRun.events]
        replayRun.records = (await this.options.replayStore?.loadRun?.(runId))?.records ?? replayRun.records
        if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
        control.close()
        throw error
      }
      context.setStatus("failed")
      emitLifecycle({ kind: "run_failed", runId, error: error instanceof Error ? error.message : String(error) })
      await flushEvents()
      replayRun.status = "failed"
      replayRun.events = [...events]
      replayRun.records = (await this.options.replayStore?.loadRun?.(runId))?.records ?? replayRun.records
      if (this.options.replayStore?.saveRun) await this.options.replayStore.saveRun(runId, replayRun)
      control.close()
      throw error
    }
  }
}

class DynamicWorkflowContextImpl<TArgs extends Record<string, unknown>> implements DynamicWorkflowContext<TArgs> {
  readonly args: Readonly<TArgs>
  private agentCount = 0
  private nextNode = 0
  private replayInvalidated = false
  private readonly phaseStack: DynamicWorkflowPhaseProgress[] = []
  private activeAgentSlots = 0
  private readonly waitingAgentSlots: Array<() => void> = []
  private nextPlanSequence = 0

  constructor(
    private readonly host: DynamicWorkflowHost,
    private readonly limits: Required<DynamicWorkflowLimits>,
    readonly progress: DynamicWorkflowProgress,
    args: TArgs,
    private readonly runId: string,
    private readonly replayActive: boolean,
    private readonly resumeRequiresRecords: boolean,
    private readonly replayNodeIds: ReadonlySet<string>,
    private readonly control: DynamicWorkflowControl,
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
    await this.control.waitIfPaused()
    return this.mapBounded(items, worker)
  }

  async pipeline<T, R>(items: readonly T[], worker: (item: T, index: number) => Promise<R> | R): Promise<R[]> {
    this.assertBatchSize(items.length, "pipeline")
    const results: R[] = []
    for (let index = 0; index < items.length; index += 1) {
      await this.control.waitIfPaused()
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
    await this.control.waitIfPaused()
    if (!prompt.trim()) throw new Error("dynamic workflow agent prompt must not be empty")
    const planSequence = this.nextPlanSequence++
    const nodeId = options.label?.trim() || `dynamic-agent-${this.nextNode++}`
    const promptFingerprint = fingerprintDynamicWorkflowInvocation(prompt, options)
    const cached = this.replayInvalidated ? undefined : await this.replayStore?.find(this.runId, nodeId, promptFingerprint)
    if (!cached && this.replayActive && this.resumeRequiresRecords && !this.replayNodeIds.has(nodeId)) {
      throw new DynamicWorkflowNothingToResumeError(`dynamic workflow invocation "${nodeId}" has no saved result`)
    }
    if (cached) {
      await this.host.recordReplayFact?.({
        runId: this.runId,
        sequence: planSequence,
        nodeId,
        promptFingerprint,
        status: cached.status,
        replay: "reused",
        resultDigest: digestDynamicWorkflowResult(cached.text),
        ...(cached.termination ? { termination: cached.termination } : {}),
      })
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
    if (this.replayActive) this.replayInvalidated = true
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
          ...(options.agent ? { agent: options.agent } : {}),
          ...(options.modelHint ? { modelHint: options.modelHint } : {}),
          ...(options.trust ? { trust: options.trust } : {}),
          ...(options.toolAccess ? { toolAccess: options.toolAccess } : {}),
          ...(options.isolation ? { isolation: options.isolation } : {}),
          ...(options.outputSchema ? { outputSchema: options.outputSchema } : {}),
          ...(options.tokenBudget !== undefined ? { tokenBudget: options.tokenBudget } : {}),
          ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
          ...(options.maxWallMs !== undefined ? { maxWallMs: options.maxWallMs } : {}),
        }],
      }
      let outcome: WorkflowOutcome
      try {
        outcome = await this.host.runWorkflow(spec, {
          runId: this.runId,
          sequence: planSequence,
          nodes: [{ nodeId, dependsOn: [], promptFingerprint, replay: "executed" }],
        })
      } catch (error) {
        await this.replayStore?.save(this.runId, {
          nodeId,
          promptFingerprint,
          text: "",
          status: "failed",
          termination: "host_error",
          error: error instanceof Error ? error.message : String(error),
        })
        await this.host.recordReplayFact?.({
          runId: this.runId,
          sequence: planSequence,
          nodeId,
          promptFingerprint,
          status: "failed",
          replay: "executed",
          resultDigest: digestDynamicWorkflowResult(""),
          termination: "host_error",
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
        await this.host.recordReplayFact?.({
          runId: this.runId,
          sequence: planSequence,
          nodeId,
          promptFingerprint,
          status: "cancelled",
          replay: "executed",
          resultDigest: digestDynamicWorkflowResult(""),
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
      await this.host.recordReplayFact?.({
        runId: this.runId,
        sequence: planSequence,
        nodeId,
        promptFingerprint,
        status,
        replay: "executed",
        resultDigest: digestDynamicWorkflowResult(text),
        ...(node?.termination ? { termination: node.termination } : { termination: "missing_node_outcome" }),
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
    await this.control.waitIfPaused()
    const planSequence = this.nextPlanSequence++
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
          ...(options.agent ? { agent: options.agent } : {}),
          ...(options.modelHint ? { modelHint: options.modelHint } : {}),
          ...(options.trust ? { trust: options.trust } : {}),
          ...(options.toolAccess ? { toolAccess: options.toolAccess } : {}),
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
        record: this.replayInvalidated ? undefined : await this.replayStore?.find(this.runId, entry.nodeId, promptFingerprint),
      }
    }))
    if (this.replayActive && this.resumeRequiresRecords) {
      const missingNode = cached.find(entry => !entry.record && !this.replayNodeIds.has(entry.nodeId))
      if (missingNode) throw new DynamicWorkflowNothingToResumeError(`dynamic workflow invocation "${missingNode.nodeId}" has no saved result`)
    }
    const firstMiss = cached.findIndex(entry => !entry.record)
    if (firstMiss >= 0 && this.replayActive) this.replayInvalidated = true
    const replayable = firstMiss >= 0 ? cached.map((entry, index) => index < firstMiss ? entry : { ...entry, record: undefined }) : cached
    const misses = replayable.filter(entry => !entry.record)
    if (this.agentCount + misses.length > this.limits.maxAgentsPerRun) {
      throw new DynamicWorkflowLimitError(
        `dynamic workflow exceeded maxAgentsPerRun (${this.limits.maxAgentsPerRun})`,
      )
    }
    const currentPhase = this.phaseStack.at(-1)
    const replayed = replayable.filter(entry => entry.record)
    if (replayed.length > 0) {
      this.progress.agentsReused += replayed.length
      this.progress.agentsCompleted += replayed.length
      if (currentPhase) {
        currentPhase.agentsStarted += replayed.length
        currentPhase.agentsCompleted += replayed.length
      }
      this.emit()
      for (const entry of replayed) {
        await this.host.recordReplayFact?.({
          runId: this.runId,
          sequence: planSequence,
          nodeId: entry.nodeId,
          promptFingerprint: entry.promptFingerprint,
          status: entry.record!.status,
          replay: "reused",
          resultDigest: digestDynamicWorkflowResult(entry.record!.text),
          ...(entry.record!.termination ? { termination: entry.record!.termination } : {}),
        })
        this.emitLifecycle?.({ kind: "agent_reused", runId: this.runId, nodeId: entry.nodeId, promptFingerprint: entry.promptFingerprint })
      }
    }
    if (misses.length === 0) {
      return replayable.map(entry => entry.record ? this.replayResult(entry.options, entry.record) : null)
    }
    this.agentCount += misses.length
    this.progress.agentsStarted = this.agentCount
    this.progress.activeAgents += misses.length
    if (currentPhase) currentPhase.agentsStarted += misses.length
    for (const entry of misses) {
      this.emitLifecycle?.({
        kind: "agent_started",
        runId: this.runId,
        nodeId: entry.nodeId,
        promptFingerprint: entry.promptFingerprint,
      })
    }
    this.emit()
    try {
      let outcome: WorkflowOutcome
      try {
        outcome = await this.host.runWorkflow({ nodes: misses.map(entry => entry.node) }, {
          runId: this.runId,
          sequence: planSequence,
          nodes: misses.map(entry => ({
            nodeId: entry.nodeId,
            dependsOn: [],
            promptFingerprint: entry.promptFingerprint,
            replay: "executed" as const,
          })),
        })
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
        await Promise.all(misses.map(entry => this.host.recordReplayFact?.({
          runId: this.runId,
          sequence: planSequence,
          nodeId: entry.nodeId,
          promptFingerprint: entry.promptFingerprint,
          status: "failed",
          replay: "executed",
          resultDigest: digestDynamicWorkflowResult(""),
          termination: "host_error",
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
        await Promise.all(misses.map(entry => this.host.recordReplayFact?.({
          runId: this.runId,
          sequence: planSequence,
          nodeId: entry.nodeId,
          promptFingerprint: entry.promptFingerprint,
          status: "cancelled",
          replay: "executed",
          resultDigest: digestDynamicWorkflowResult(""),
          termination: "workflow_rejected",
        })))
        for (const entry of misses) {
          this.emitLifecycle?.({
            kind: "agent_completed",
            runId: this.runId,
            nodeId: entry.nodeId,
            status: "failed",
            termination: "workflow_rejected",
          })
          this.progress.agentsCompleted += 1
          if (currentPhase) currentPhase.agentsCompleted += 1
        }
        return replayable.map(entry => entry.record ? this.replayResult(entry.options, entry.record) : null)
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
        await this.host.recordReplayFact?.({
          runId: this.runId,
          sequence: planSequence,
          nodeId: entry.nodeId,
          promptFingerprint: entry.promptFingerprint,
          status,
          replay: "executed",
          resultDigest: digestDynamicWorkflowResult(text),
          ...(node?.termination ? { termination: node.termination } : { termination: "missing_node_outcome" }),
        })
        this.emitLifecycle?.({ kind: "agent_completed", runId: this.runId, nodeId: entry.nodeId, status, ...(node?.termination ? { termination: node.termination } : { termination: "missing_node_outcome" }) })
        this.progress.agentsCompleted += 1
        if (currentPhase) currentPhase.agentsCompleted += 1
      }
      return replayable.map(entry => entry.record ? this.replayResult(entry.options, entry.record) : results.get(entry.nodeId) ?? null)
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

function digestDynamicWorkflowResult(text: string): string {
  return `sha256:${createHash("sha256").update(text).digest("hex")}`
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

async function raceDynamicWorkflowCancellation<T>(work: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return work
  if (signal.aborted) throw new DynamicWorkflowCancellationError(signal.reason instanceof Error ? signal.reason.message : undefined)
  let onAbort: (() => void) | undefined
  const cancellation = new Promise<never>((_, reject) => {
    onAbort = () => reject(new DynamicWorkflowCancellationError(signal.reason instanceof Error ? signal.reason.message : undefined))
    signal.addEventListener("abort", onAbort, { once: true })
  })
  try {
    return await Promise.race([work, cancellation])
  } finally {
    if (onAbort) signal.removeEventListener("abort", onAbort)
  }
}
