import { normalizeAgent } from "./agent-ir.js"
import { type AgentOptions, type ModelRef } from "./agent.js"
import { InMemorySessionLog, type SessionLog } from "./runtime/session-log.js"
import { LocalExecutionPlane, type ExecutionPlane } from "./runtime/execution-plane.js"
import { RuntimeRunner, type RuntimeOptions } from "./runtime/runner.js"
import type { LLMProvider, StreamEvent, DoneEvent, ErrorEvent, TokenUsage, ContentPart } from "./types.js"
import type { RegisteredTool } from "./tools/index.js"
import type { MemoryRecord, MemoryRecall, MemoryQuery, MemoryScope, MemoryStore, MemoryKind } from "./memory/protocols.js"
import type { WorkflowSpec, WorkflowOutcome, KernelAgentRole } from "./types/agent.js"
import { extractJsonValue, schemaInstruction, validateAgainstSchema } from "./runtime/output-schema.js"
import type { GovernancePolicy } from "./governance.js"

export interface AgentDefinition extends Omit<AgentOptions, "model" | "name"> {
  name?: string
  /** Public model identity. Runtime resolves this through a provider binding. */
  model?: ModelRef
  /** Optional host binding retained for local/custom execution. */
  provider?: LLMProvider
  tools?: RegisteredTool[]
  executionPlane?: ExecutionPlane
  sessionLog?: SessionLog
  maxTokens?: number
  memoryStore?: MemoryStore
  memoryScope?: MemoryScope
  runtimeOptions?: Pick<RuntimeOptions, "memoryPolicy" | "governancePolicy" | "signalSource" | "signalPolicy" | "resourceQuota" | "onPermissionRequest" | "payloadStore" | "runGroup" | "subAgentOrchestrator" | "reducers" | "providerFor" | "initialMemory">
}

export interface AgentRunOptions {
  session?: SessionRef
  maxTurns?: number
  signal?: AbortSignal
  metadata?: Record<string, unknown>
  onPermissionRequest?: RuntimeOptions["onPermissionRequest"]
  /** Multimodal user input attached to this run and persisted in the session log. */
  attachments?: ContentPart[]
}

export interface SessionRef {
  id: string
}

export interface RunResult<T = string> {
  output: T
  runId: string
  sessionId: string
  status: "completed" | "partial" | "failed" | "cancelled"
  usage?: TokenUsage
  outputValidation?: { ok: boolean; errors: string[] }
}

export interface AgentSession extends SessionRef {
  run(goal: string, options?: Omit<AgentRunOptions, "session">): Promise<RunResult>
  stream(goal: string, options?: Omit<AgentRunOptions, "session">): AsyncIterable<StreamEvent>
  resume(options?: Omit<AgentRunOptions, "session">): AsyncIterable<StreamEvent>
  interrupt(reason?: "user" | "deadline" | "lease_lost" | "host_shutdown"): void
}

export interface MemoryInput {
  name: string
  content: string
  description?: string
  kind?: MemoryKind
  confidence?: number
  pinned?: boolean
  ttlDays?: number
}

export interface RecallOptions {
  topK?: number
  kinds?: MemoryKind[]
  minScore?: number
}

export interface DelegationRequest {
  goal: string
  role?: KernelAgentRole
}

export interface DelegationResult {
  output: string
  status: "completed" | "partial" | "failed"
  nodeId?: string
}

/** The executable host handle created from an AgentDefinition. */
export interface AgentRuntime {
  readonly name: string
  readonly definition: Readonly<AgentDefinition>
  run(goal: string, options?: AgentRunOptions): Promise<RunResult>
  stream(goal: string, options?: AgentRunOptions): AsyncIterable<StreamEvent>
  session(id?: string): AgentSession
  remember(input: MemoryInput): Promise<MemoryRecord>
  recall(query: string, options?: RecallOptions): Promise<MemoryRecall[]>
  delegate(request: DelegationRequest): Promise<DelegationResult>
  workflow(spec: WorkflowSpec, options?: { session?: SessionRef }): Promise<WorkflowOutcome>
  listen(options?: { session?: SessionRef; leaseMs?: number }): Promise<RunResult | null>
}

function sessionId(ref?: SessionRef): string {
  return ref?.id ?? `session-${crypto.randomUUID()}`
}

function statusFromDone(status: string): RunResult["status"] {
  if (status === "completed" || status === "done") return "completed"
  if (status === "cancelled" || status === "user" || status === "deadline" || status === "lease_lost" || status === "host_shutdown") return "cancelled"
  if (status === "failed" || status === "error") return "failed"
  return "partial"
}

function declarativeContextSeeds(definition: AgentDefinition): string[] {
  const seeds: string[] = []
  for (const skill of definition.skills ?? []) {
    if (skill.instructions) seeds.push(`[Skill: ${skill.name}]\n${skill.instructions}`)
    for (const entry of skill.knowledge ?? []) {
      if (typeof entry === "string") seeds.push(`[Skill knowledge: ${skill.name}]\n${entry}`)
      else if (entry.content) seeds.push(`[Skill knowledge: ${entry.name}]\n${entry.content}`)
    }
  }
  for (const item of definition.knowledge ?? []) {
    if (item.source.kind === "text") seeds.push(item.name ? `[Knowledge: ${item.name}]\n${item.source.content}` : item.source.content)
  }
  return seeds
}

function mergeGuardrailPolicies(
  base: GovernancePolicy | undefined,
  guardrails: AgentDefinition["guardrails"] | undefined,
): GovernancePolicy | undefined {
  const policies = [base, ...(guardrails ?? []).map(guardrail => guardrail.policy)].filter(
    (policy): policy is GovernancePolicy => policy !== undefined,
  )
  if (!policies.length) return undefined
  return {
    ...(policies.some(policy => policy.defaultAction === "deny") ? { defaultAction: "deny" as const } : {}),
    rules: policies.flatMap(policy => policy.rules ?? []),
    vetoes: [...new Set(policies.flatMap(policy => policy.vetoes ?? []))],
    rateLimits: policies.flatMap(policy => policy.rateLimits ?? []),
    constraints: policies.flatMap(policy => policy.constraints ?? []),
    ...(policies.some(policy => policy.surfaceDeniedInSystem === false) ? { surfaceDeniedInSystem: false } : {}),
  }
}

class AgentSessionImpl {
  constructor(private readonly owner: AgentRuntimeImpl, public readonly id: string) {}

  run(goal: string, options?: Omit<AgentRunOptions, "session">): Promise<RunResult> {
    return this.owner.run(goal, { ...options, session: { id: this.id } })
  }

  stream(goal: string, options?: Omit<AgentRunOptions, "session">): AsyncIterable<StreamEvent> {
    return this.owner.stream(goal, { ...options, session: { id: this.id } })
  }

  resume(options?: Omit<AgentRunOptions, "session">): AsyncIterable<StreamEvent> {
    return this.owner.resume(this.id, options)
  }

  interrupt(reason: "user" | "deadline" | "lease_lost" | "host_shutdown" = "user"): void {
    this.owner.interrupt(reason)
  }
}

class AgentRuntimeImpl implements AgentRuntime {
  readonly name: string
  readonly definition: Readonly<AgentDefinition>
  private readonly sessionLog: SessionLog
  private activeRunner: RuntimeRunner | null = null

  constructor(definition: AgentDefinition) {
    this.definition = Object.freeze({ ...definition })
    this.name = normalizeAgent(definition).name
    this.sessionLog = definition.sessionLog ?? new InMemorySessionLog()
  }

  session(id = `session-${crypto.randomUUID()}`): AgentSession {
    return new AgentSessionImpl(this, id)
  }

  async remember(input: MemoryInput): Promise<MemoryRecord> {
    const store = this.definition.memoryStore
    const scope = this.definition.memoryScope
    if (!store || !scope) throw new Error("agent memory requires memoryStore and memoryScope")
    const now = Date.now()
    const record: MemoryRecord = {
      record_id: crypto.randomUUID(),
      scope,
      name: input.name,
      kind: input.kind ?? "reference",
      content: input.content,
      description: input.description ?? "",
      provenance: { author: "host", trust: "user_asserted", evidence_refs: [] },
      created_at: now,
      updated_at: now,
      recall_count: 0,
      confidence: input.confidence ?? 1,
      links: [],
      pinned: input.pinned ?? false,
      ...(input.ttlDays !== undefined ? { ttl_days: input.ttlDays } : {}),
    }
    await store.put(this.name, record)
    return record
  }

  async recall(query: string, options: RecallOptions = {}): Promise<MemoryRecall[]> {
    const store = this.definition.memoryStore
    const scope = this.definition.memoryScope
    if (!store || !scope) throw new Error("agent memory requires memoryStore and memoryScope")
    const request: MemoryQuery = {
      scope,
      query,
      top_k: options.topK ?? 8,
      kinds: options.kinds ?? [],
      ...(options.minScore !== undefined ? { min_score: options.minScore } : {}),
    }
    return store.search(this.name, request)
  }

  async delegate(request: DelegationRequest): Promise<DelegationResult> {
    const spec: WorkflowSpec = {
      nodes: [{
        task: { goal: request.goal },
        role: request.role ?? "explore",
        isolation: "read_only",
        contextInheritance: "system_only",
      }],
    }
    const outcome = await this.workflow(spec)
    const node = outcome.nodeOutcomes[0]
    const nodeId = node?.nodeId
    return {
      output: nodeId ? outcome.outputs[nodeId] ?? "" : "",
      status: node?.status === "completed" ? "completed" : node?.status === "failed" ? "failed" : "partial",
      ...(nodeId ? { nodeId } : {}),
    }
  }

  async workflow(spec: WorkflowSpec, options: { session?: SessionRef } = {}): Promise<WorkflowOutcome> {
    const runner = this.createRunner({})
    this.activeRunner = runner
    try {
      return await runner.runWorkflow(spec, { sessionId: sessionId(options.session) })
    } finally {
      this.activeRunner = null
    }
  }

  async listen(options: { session?: SessionRef; leaseMs?: number } = {}): Promise<RunResult | null> {
    const source = this.definition.runtimeOptions?.signalSource
    if (!source) throw new Error("agent signals require runtimeOptions.signalSource")
    const claim = await source.claimSignal(this.name, options.leaseMs)
    if (!claim) return null
    const payload = claim.signal.payload
    const goal = typeof payload.goal === "string"
      ? payload.goal
      : typeof payload.summary === "string"
        ? payload.summary
        : JSON.stringify(payload)
    try {
      const result = await this.run(goal, options.session ? { session: options.session } : {})
      await source.ackSignal(claim)
      return result
    } catch (error) {
      await source.nackSignal(claim)
      throw error
    }
  }

  stream(goal: string, options: AgentRunOptions = {}): AsyncIterable<StreamEvent> {
    const session = sessionId(options.session)
    const runner = this.createRunner(options)
    this.activeRunner = runner
    const abort = () => runner.interrupt("user")
    if (options.signal) {
      if (options.signal.aborted) runner.interrupt("user")
      else options.signal.addEventListener("abort", abort, { once: true })
    }
    const stream = runner.run({ sessionId: session, goal, ...(options.attachments?.length ? { attachments: options.attachments } : {}) })
    return this.clearRunnerAfter(stream, options.signal, abort)
  }

  async run(goal: string, options: AgentRunOptions = {}): Promise<RunResult> {
    const session = sessionId(options.session)
    const events: StreamEvent[] = []
    for await (const event of this.stream(goal, { ...options, session: { id: session } })) events.push(event)
    const done = [...events].reverse().find(event => event.type === "done") as DoneEvent | undefined
    const error = [...events].reverse().find(event => event.type === "error") as ErrorEvent | undefined
    const persisted = await this.sessionLog.read(session)
    const started = [...persisted].reverse().find(entry => entry.event.kind === "run_started")
    const usageEvent = [...events].reverse().find(event => event.type === "usage") as (StreamEvent & Partial<TokenUsage>) | undefined
    const output = events.filter(event => event.type === "text_delta").map(event => String((event as { delta?: unknown }).delta ?? "")).join("")
    const outputValidation = this.definition.outputSchema
      ? validateAgainstSchema(extractJsonValue(output), this.definition.outputSchema)
      : undefined
    return {
      output,
      runId: started?.event.kind === "run_started" ? started.event.run_id : `run-${crypto.randomUUID()}`,
      sessionId: session,
      status: error || outputValidation && !outputValidation.ok ? "failed" : statusFromDone(done?.status ?? "partial"),
      ...(outputValidation ? { outputValidation } : {}),
      ...(usageEvent?.totalTokens !== undefined ? {
        usage: {
          inputTokens: usageEvent.inputTokens ?? 0,
          outputTokens: usageEvent.outputTokens ?? 0,
          totalTokens: usageEvent.totalTokens,
        },
      } : {}),
    }
  }

  async *resume(id: string, options: Omit<AgentRunOptions, "session"> = {}): AsyncIterable<StreamEvent> {
    const runner = this.createRunner(options)
    this.activeRunner = runner
    yield* this.clearRunnerAfter(runner.wake(id), options.signal, () => runner.interrupt("user"))
  }

  interrupt(reason: "user" | "deadline" | "lease_lost" | "host_shutdown" = "user"): void {
    this.activeRunner?.interrupt(reason)
  }

  private createRunner(options: AgentRunOptions): RuntimeRunner {
    const model = this.definition.model
    const provider = this.definition.provider
      ?? (typeof model === "string" ? this.definition.runtimeOptions?.providerFor?.(model) : undefined)
    if (!provider) {
      throw new Error(`agent "${this.name}" has no runtime provider binding for model ${typeof this.definition.model === "string" ? this.definition.model : "(unresolved)"}`)
    }
    const plane = this.definition.executionPlane
      ?? (this.definition.tools ?? []).reduce((current, currentTool) => current.register(currentTool), new LocalExecutionPlane())
    const runtime: RuntimeOptions = {
      provider,
      ...(mergeGuardrailPolicies(this.definition.runtimeOptions?.governancePolicy, this.definition.guardrails)
        ? { governancePolicy: mergeGuardrailPolicies(this.definition.runtimeOptions?.governancePolicy, this.definition.guardrails) }
        : {}),
      ...(this.definition.capabilityFilter ? { capabilityFilter: this.definition.capabilityFilter } : {}),
      executionPlane: plane,
      sessionLog: this.sessionLog,
      maxTokens: this.definition.maxTokens ?? 32_000,
      ...(this.definition.instructions || this.definition.outputSchema ? {
        systemPrompt: [
          this.definition.instructions,
          this.definition.outputSchema ? schemaInstruction(this.definition.outputSchema) : undefined,
        ].filter((part): part is string => Boolean(part)).join("\n\n"),
      } : {}),
      ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
      ...(this.definition.memoryStore ? { memoryStore: this.definition.memoryStore } : {}),
      ...(this.definition.memoryScope ? { memoryScope: this.definition.memoryScope } : {}),
      ...((this.definition.runtimeOptions?.initialMemory?.length || declarativeContextSeeds(this.definition).length) ? {
        initialMemory: [
          ...(this.definition.runtimeOptions?.initialMemory ?? []),
          ...declarativeContextSeeds(this.definition),
        ],
      } : {}),
      agentId: this.name,
      ...(this.definition.runtimeOptions ?? {}),
      ...(options.onPermissionRequest ? { onPermissionRequest: options.onPermissionRequest } : {}),
    }
    return new RuntimeRunner(runtime)
  }

  private async *clearRunnerAfter(stream: AsyncIterable<StreamEvent>, signal?: AbortSignal, abort?: () => void): AsyncIterable<StreamEvent> {
    try {
      yield* stream
    } finally {
      if (signal && abort) signal.removeEventListener("abort", abort)
      this.activeRunner = null
    }
  }
}

export function createAgent(definition: AgentDefinition): AgentRuntime {
  return new AgentRuntimeImpl(definition)
}
