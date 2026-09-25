import type { AgentCapabilityFilter, WorkflowOutcome, WorkflowSpec } from "./runtime/types/agent.js"
import type { Memory, WorkingMemory, MemoryRecord, MemoryRecall, MemoryKind } from "./memory/index.js"
import type { RegisteredTool } from "./tools/index.js"
import type { LLMProvider, StreamEvent, ContentPart, UsageEvent } from "./types.js"
import { RuntimeRunner } from "./runtime/runner.js"
import { LocalExecutionPlane, type ExecutionPlane } from "./runtime/execution-plane.js"
import { InMemorySessionLog, type SessionEvent, type SessionLog } from "./runtime/session-log.js"
import { extractJsonValue, validateAgainstSchema } from "./runtime/output-schema.js"
import type { SignalSource } from "./signals/index.js"
import type { McpExecutionPlane } from "./runtime/mcp-transport.js"

type JsonSchema = Record<string, unknown>
export interface MemoryReference { kind?: "durable"; namespace?: string }
export type AgentMemory = Memory | WorkingMemory | MemoryReference
export type ModelRef = string | ModelRequirement
export interface ModelRequirement { capability?: { reasoning?: boolean; vision?: boolean; toolUse?: boolean }; contextWindow?: number; latencyClass?: string; costClass?: string }
export interface AgentToolDefinition { name: string; description?: string; parameters?: Record<string, unknown>; providerOptions?: Record<string, unknown> }
export type McpTransport = { kind: "stdio"; command: string; args?: string[] } | { kind: "http" | "sse"; url: string } | { kind: "custom"; [key: string]: unknown }
export interface MCPServer { name?: string; transport: McpTransport; tools?: string[]; resources?: boolean; prompts?: boolean; auth?: Record<string, unknown>; metadata?: Record<string, unknown>; providerOptions?: Record<string, unknown> }
export interface Skill { name: string; description?: string; instructions?: string; resources?: unknown[]; scripts?: unknown[]; tools?: unknown[]; mcpServers?: unknown[]; knowledge?: unknown[]; metadata?: Record<string, unknown>; providerOptions?: Record<string, unknown> }
export type KnowledgeSourceRef = { kind: "file" | "directory" | "text" | "url" | "vector" | "custom"; [key: string]: unknown }
export interface Knowledge { id?: string; name?: string; source: KnowledgeSourceRef; description?: string; metadata?: Record<string, unknown>; providerOptions?: Record<string, unknown> }
export type AgentRef = string | { name: string }
export interface Handoff { agent: AgentRef; description?: string; inputSchema?: JsonSchema; metadata?: Record<string, unknown>; providerOptions?: Record<string, unknown> }
export interface Guardrail { name: string; description?: string; metadata?: Record<string, unknown> }

export interface AgentRuntimeBinding {
  provider?: LLMProvider
  providerFor?: (model: string) => LLMProvider | undefined
  runtimeOptions?: Partial<import("./runtime/runner.js").RuntimeOptions>
  sessionLog?: SessionLog
  executionPlane?: ExecutionPlane
  mcpPlane?: McpExecutionPlane
  resolveAgent?: (name: string) => Agent | undefined | Promise<Agent | undefined>
  signalSource?: SignalSource
}
export interface AgentOptions {
  name: string; description?: string; instructions?: string; model?: ModelRef
  capabilityFilter?: AgentCapabilityFilter; tools?: Array<RegisteredTool | AgentToolDefinition>
  mcpServers?: MCPServer[]; skills?: Skill[]; memory?: AgentMemory; knowledge?: Knowledge[]
  handoffs?: Handoff[]; providerOptions?: Record<string, unknown>; outputSchema?: JsonSchema
  metadata?: Record<string, unknown>; guardrails?: Guardrail[]; runtimeBinding?: AgentRuntimeBinding
}
export interface AgentRunOptions {
  sessionId?: string; maxTurns?: number; maxTotalTokens?: number; timeoutMs?: number
  metadata?: Record<string, unknown>; providerOptions?: Record<string, unknown>
  attachments?: ContentPart[]; cancelSignal?: AbortSignal
}
export interface AgentRunResult {
  output: string; runId?: string; sessionId: string
  status: "completed" | "partial" | "failed" | "cancelled"
  usage?: { inputTokens: number; outputTokens: number; totalTokens: number }
  outputValidation?: { ok: boolean; errors: string[] }
  evidence?: { route?: unknown; measurement?: unknown; contextBinding?: unknown }
}
export interface MemoryInput { name: string; content: string; description?: string; kind?: MemoryKind; confidence?: number; pinned?: boolean; ttlDays?: number }
export interface RecallOptions { topK?: number; kinds?: MemoryKind[]; minScore?: number }
export interface DelegationResult { output: string; status: "completed" | "partial" | "failed" }

function statusOf(reason: string): AgentRunResult["status"] {
  if (["completed", "success", "done"].includes(reason)) return "completed"
  if (["cancelled", "user", "user_abort", "deadline", "timeout"].includes(reason)) return "cancelled"
  if (["failed", "error", "invalid_arg"].includes(reason)) return "failed"
  return "partial"
}

export class AgentSession {
  readonly id: string
  private activeRunner?: RuntimeRunner
  constructor(private readonly owner: Agent, id: string) { this.id = id }
  history(fromSeq = 0) { return this.owner.sessionLog.read(this.id, fromSeq) }
  latestSeq() { return this.owner.sessionLog.latestSeq(this.id) }
  async workflowTrace() {
    const kinds = new Set(["workflow_batch_spawned", "workflow_node_completed", "workflow_nodes_submitted", "workflow_completed"])
    return (await this.history()).filter(entry => kinds.has(entry.event.kind))
  }
  async workflowReplay() {
    const events = await this.workflowTrace()
    const outcomes = new Map<string, Record<string, unknown>>()
    let completed = false
    for (const entry of events) {
      if (entry.event.kind === "workflow_node_completed") {
        const id = entry.event.agent_id
        if (id) outcomes.set(id, entry.event as unknown as Record<string, unknown>)
      }
      if (entry.event.kind === "workflow_completed") {
        completed = true
        for (const outcome of entry.event.node_outcomes) {
          const id = outcome.node_id
          if (id) outcomes.set(id, outcome as unknown as Record<string, unknown>)
        }
      }
    }
    return { events, nodeOutcomes: [...outcomes.values()], completed }
  }
  private runner(goal: string, options: AgentRunOptions = {}) { return this.owner.createRunner(goal, this.id, options) }

  async *stream(goal: string, options: Omit<AgentRunOptions, "sessionId"> = {}): AsyncIterable<StreamEvent> {
    const runner = await this.runner(goal, { ...options, sessionId: this.id })
    this.activeRunner = runner
    const abort = () => runner.interrupt("user")
    if (options.cancelSignal?.aborted) abort()
    else options.cancelSignal?.addEventListener("abort", abort, { once: true })
    try { yield* runner.run({ sessionId: this.id, goal, attachments: options.attachments, extensions: options.providerOptions }) }
    finally { options.cancelSignal?.removeEventListener("abort", abort); this.activeRunner = undefined }
  }
  async run(goal: string, options: Omit<AgentRunOptions, "sessionId"> = {}) {
    const events: StreamEvent[] = []
    for await (const event of this.stream(goal, options)) events.push(event)
    return this.owner.result(this.id, events)
  }
  async *resume(options: Omit<AgentRunOptions, "sessionId"> = {}): AsyncIterable<StreamEvent> {
    const runner = await this.runner("resume", { ...options, sessionId: this.id })
    this.activeRunner = runner
    try { yield* runner.wake(this.id, options.providerOptions) }
    finally { this.activeRunner = undefined }
  }
  interrupt(reason: "user" | "deadline" | "lease_lost" | "host_shutdown" = "user") { this.activeRunner?.interrupt(reason) }
  remember(input: MemoryInput): Promise<MemoryRecord> { return this.owner.remember(input, this.id) }
  recall(query: string, options: RecallOptions = {}): Promise<MemoryRecall[]> { return this.owner.recall(query, options, this.id) }
  async workflow(spec: WorkflowSpec): Promise<WorkflowOutcome> {
    const runner = await this.runner("workflow", { sessionId: this.id }); this.activeRunner = runner
    try { return await runner.runWorkflow(spec, { sessionId: this.id }) } finally { this.activeRunner = undefined }
  }
}

export class Agent {
  readonly name: string; readonly description?: string; readonly instructions?: string; readonly model?: ModelRef
  readonly capabilityFilter?: AgentCapabilityFilter; readonly tools?: Array<RegisteredTool | AgentToolDefinition>
  readonly mcpServers?: MCPServer[]; readonly skills?: Skill[]; readonly memory?: AgentMemory; readonly knowledge?: Knowledge[]
  readonly handoffs?: Handoff[]; readonly providerOptions?: Record<string, unknown>; readonly outputSchema?: JsonSchema
  readonly metadata?: Record<string, unknown>; readonly guardrails?: Guardrail[]; readonly runtimeBinding?: AgentRuntimeBinding
  readonly sessionLog: SessionLog
  private readonly sessions = new Map<string, AgentSession>()
  constructor(options: AgentOptions) {
    this.name = options.name; this.description = options.description; this.instructions = options.instructions; this.model = options.model
    this.capabilityFilter = options.capabilityFilter; this.tools = options.tools; this.mcpServers = options.mcpServers
    this.skills = options.skills; this.memory = options.memory; this.knowledge = options.knowledge; this.handoffs = options.handoffs
    this.providerOptions = options.providerOptions; this.outputSchema = options.outputSchema; this.metadata = options.metadata; this.guardrails = options.guardrails
    this.runtimeBinding = options.runtimeBinding; this.sessionLog = options.runtimeBinding?.sessionLog ?? options.runtimeBinding?.runtimeOptions?.sessionLog ?? new InMemorySessionLog()
  }
  session(id = `session-${crypto.randomUUID()}`) { if (!this.sessions.has(id)) this.sessions.set(id, new AgentSession(this, id)); return this.sessions.get(id)! }
  async createRunner(_goal: string, _sessionId: string, options: AgentRunOptions = {}) {
    const binding = this.runtimeBinding
    const provider = binding?.provider ?? (typeof this.model === "string" ? binding?.providerFor?.(this.model) : undefined)
    if (!provider) throw new Error(`agent "${this.name}" has no runtime provider binding`)
    const base = { ...(binding?.runtimeOptions ?? {}) } as Record<string, unknown>
    if (options.maxTurns !== undefined) base.maxTurns = options.maxTurns
    if (options.maxTotalTokens !== undefined) base.maxTotalTokens = options.maxTotalTokens
    if (options.timeoutMs !== undefined) base.timeoutMs = options.timeoutMs
    if (options.providerOptions) base.extensions = { ...((base.extensions as Record<string, unknown> | undefined) ?? {}), ...options.providerOptions }
    const mcpPlane = binding?.mcpPlane
    if (mcpPlane) await mcpPlane.connect()
    return new RuntimeRunner({ provider, executionPlane: binding?.executionPlane ?? mcpPlane ?? new LocalExecutionPlane(), sessionLog: this.sessionLog, maxTokens: 32_000, agentId: this.name, ...(this.instructions ? { systemPrompt: this.instructions } : {}), ...base } as import("./runtime/runner.js").RuntimeOptions)
  }
  async run(goal: string, options: AgentRunOptions = {}) { return this.session(options.sessionId).run(goal, options) }
  stream(goal: string, options: AgentRunOptions = {}) { return this.session(options.sessionId).stream(goal, options) }
  resume(sessionId: string, options: Omit<AgentRunOptions, "sessionId"> = {}) { return this.session(sessionId).resume(options) }
  interrupt(reason: "user" | "deadline" | "lease_lost" | "host_shutdown" = "user", sessionId?: string) { sessionId ? this.session(sessionId).interrupt(reason) : [...this.sessions.values()].forEach(s => s.interrupt(reason)) }
  workflow(spec: WorkflowSpec, options: { sessionId?: string } = {}) { return this.session(options.sessionId).workflow(spec) }
  async remember(input: MemoryInput, sessionId?: string): Promise<MemoryRecord> {
    if (!this.memory || typeof (this.memory as Memory).put !== "function") throw new Error(`agent "${this.name}" memory is not runtime-bound`)
    const now = Date.now()
    const record: MemoryRecord = {
      record_id: crypto.randomUUID(), scope: (this.memory as Memory & { scope?: { tenant_id: string; namespace: string } }).scope
        ?? { tenant_id: "default", namespace: (this.memory as Memory).namespace ?? this.name },
      name: input.name, kind: input.kind ?? "reference", content: input.content,
      description: input.description ?? input.name,
      provenance: { author: "host", trust: "user_asserted", evidence_refs: [], session_id: sessionId ?? `memory-${crypto.randomUUID()}` },
      created_at: now, updated_at: now, recall_count: 0, confidence: input.confidence ?? 1,
      links: [], pinned: input.pinned ?? false, ...(input.ttlDays === undefined ? {} : { ttl_days: input.ttlDays }),
    }
    await (this.memory as Memory).put(record)
    return record
  }
  async recall(query: string, options: RecallOptions = {}, _sessionId?: string): Promise<MemoryRecall[]> {
    if (!this.memory || typeof (this.memory as Memory).search !== "function") throw new Error(`agent "${this.name}" memory is not runtime-bound`)
    const records = await (this.memory as Memory).search(query, { topK: options.topK ?? 8, kinds: options.kinds, minScore: options.minScore })
    return records.map(record => ({ record, score: 1, why: "memory facade search" }))
  }
  async delegate(request: { target: AgentRef; goal: string; metadata?: Record<string, unknown>; providerOptions?: Record<string, unknown> }): Promise<DelegationResult> {
    const targetName = typeof request.target === "string" ? request.target : request.target.name
    const declared = (this.handoffs ?? []).some(h => (typeof h.agent === "string" ? h.agent : h.agent.name) === targetName)
    if (!declared) throw new Error(`agent "${this.name}" cannot hand off to "${targetName}"`)
    const resolver = this.runtimeBinding?.resolveAgent
    if (!resolver) throw new Error(`agent "${this.name}" requires a host target resolver`)
    const target = await resolver(targetName)
    if (!target) throw new Error(`target agent "${targetName}" is not registered`)
    const result = await target.run(request.goal, { metadata: request.metadata, providerOptions: request.providerOptions })
    return { output: result.output, status: result.status === "cancelled" ? "partial" : result.status }
  }
  async listen(): Promise<AgentRunResult | null> {
    const source = this.runtimeBinding?.signalSource
    if (!source) throw new Error("agent signals require runtimeBinding.signalSource")
    const claim = await source.claimSignal()
    if (!claim) return null
    const goal = String(claim.signal.payload.goal ?? claim.signal.payload.summary ?? JSON.stringify(claim.signal.payload))
    try { const result = await this.run(goal); await source.ackSignal(claim); return result }
    catch (error) { await source.nackSignal(claim); throw error }
  }
  async close() { await this.runtimeBinding?.mcpPlane?.disconnect() }
  async result(sessionId: string, events: StreamEvent[]): Promise<AgentRunResult> {
    const entries = await this.sessionLog.read(sessionId)
    const started = [...entries].reverse().find(e => e.event.kind === "run_started")?.event
    const terminal = [...entries].reverse().find(e => e.event.kind === "run_terminal")?.event
    const output = events.filter((e): e is StreamEvent & { delta: unknown } => e.type === "text_delta" && "delta" in e).map(e => String(e.delta)).join("")
    const usage = [...events].reverse().find((e): e is UsageEvent => e.type === "usage")
    const result: AgentRunResult = { output, runId: started?.kind === "run_started" ? started.run_id : undefined, sessionId, status: statusOf(terminal?.kind === "run_terminal" ? terminal.reason : events.some(e => e.type === "error") ? "error" : "completed") }
    const attempt = [...entries].reverse().find(e => e.event.kind === "provider_attempt")?.event
    if (usage) result.usage = { inputTokens: usage.inputTokens ?? 0, outputTokens: usage.outputTokens ?? 0, totalTokens: usage.totalTokens }
    else if (attempt?.kind === "provider_attempt" && attempt.usage) {
      result.usage = { inputTokens: attempt.usage.inputTokens, outputTokens: attempt.usage.outputTokens, totalTokens: attempt.usage.inputTokens + attempt.usage.outputTokens }
    }
    const measured = [...entries].reverse().find(e => e.event.kind === "prompt_measured")?.event
    const prepared = [...entries].reverse().find(e => e.event.kind === "context_prepared")?.event
    if ((started?.kind === "run_started" && started.route) || (attempt?.kind === "provider_attempt" && attempt.route) || measured || prepared) {
      result.evidence = {
        ...((attempt?.kind === "provider_attempt" ? attempt.route : started?.kind === "run_started" ? started.route : undefined) ? { route: attempt?.kind === "provider_attempt" ? attempt.route : (started as Extract<SessionEvent, { kind: "run_started" }>).route } : {}),
        ...(measured?.kind === "prompt_measured" ? { measurement: measured.measurement } : {}),
        ...(prepared?.kind === "context_prepared" ? { contextBinding: prepared.preparation.binding } : {}),
      }
    }
    if (this.outputSchema) {
      result.outputValidation = validateAgainstSchema(extractJsonValue(output), this.outputSchema)
      if (!result.outputValidation.ok && result.status === "completed") result.status = "failed"
    }
    return result
  }
}
