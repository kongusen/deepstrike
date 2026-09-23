import { type AgentOptions, type ModelRef } from "./agent.js"
import { InMemorySessionLog, type SessionLog } from "./runtime/session-log.js"
import { LocalExecutionPlane, type ExecutionPlane } from "./runtime/execution-plane.js"
import { RuntimeRunner, type RuntimeOptions } from "./runtime/runner.js"
import type { LLMProvider, StreamEvent, DoneEvent, ErrorEvent, TokenUsage, ContentPart } from "./types.js"
import type { RegisteredTool } from "./tools/index.js"
import type { MemoryRecord, MemoryRecall, MemoryQuery, MemoryScope, MemoryStore, MemoryKind } from "./memory/protocols.js"
import type { WorkflowSpec, WorkflowOutcome, KernelAgentRole } from "./types/agent.js"
import { extractJsonValue, validateAgainstSchema } from "./runtime/output-schema.js"
import { McpProxyPlane } from "./runtime/mcp-proxy-plane.js"
import { EnvCredentialVault } from "./runtime/credential-vault.js"
import { agentRefName } from "./handoff-target.js"
import { buildAgentRuntimeOptions } from "./runtime/agent-runtime-options.js"
import { captureAgentDeclaration, materializeAgentDefinition, type AgentDeclaration, type AgentHostBindings } from "./runtime/agent-declaration.js"

export type { AgentDeclaration } from "./runtime/agent-declaration.js"

export interface AgentDefinition extends Omit<AgentOptions, "model" | "name"> {
  name?: string
  /** Public model identity. Runtime resolves this through a provider binding. */
  model?: ModelRef
  tools?: RegisteredTool[]
  maxTokens?: number
  memoryStore?: MemoryStore
  memoryScope?: MemoryScope
  runtimeBinding?: RuntimeBinding
}

export interface RuntimeBinding {
  provider?: LLMProvider
  providerFor?: RuntimeOptions["providerFor"]
  executionPlane?: ExecutionPlane
  sessionLog?: SessionLog
  runtimeOptions?: Pick<RuntimeOptions, "memoryPolicy" | "governancePolicy" | "signalSource" | "signalPolicy" | "resourceQuota" | "onPermissionRequest" | "payloadStore" | "runGroup" | "subAgentOrchestrator" | "reducers" | "initialMemory" | "skillCatalog" | "knowledgeSource" | "contextManager" | "artifactSetDigest">
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
  /** Host-owned execution evidence captured for evaluation and replay. */
  evidence?: {
    contextBinding?: unknown
    route?: unknown
    measurement?: unknown
    artifactSet?: unknown
  }
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
  /** Optional declared handoff target. When handoffs are declared, this is required and allowlisted. */
  target?: import("./handoff-target.js").AgentRef
}

export interface DelegationResult {
  output: string
  status: "completed" | "partial" | "failed"
  nodeId?: string
}

/** The executable public Agent handle created from an AgentDefinition. */
export interface Agent {
  readonly name: string
  readonly declaration: AgentDeclaration
  readonly definition: Readonly<AgentDefinition>
  run(goal: string, options?: AgentRunOptions): Promise<RunResult>
  stream(goal: string, options?: AgentRunOptions): AsyncIterable<StreamEvent>
  session(id?: string): AgentSession
  remember(input: MemoryInput): Promise<MemoryRecord>
  recall(query: string, options?: RecallOptions): Promise<MemoryRecall[]>
  delegate(request: DelegationRequest): Promise<DelegationResult>
  workflow(spec: WorkflowSpec, options?: { session?: SessionRef }): Promise<WorkflowOutcome>
  listen(options?: { session?: SessionRef; leaseMs?: number }): Promise<RunResult | null>
  close(): Promise<void>
}

/** @internal Compatibility alias; public code should use `Agent`. */
export type AgentRuntime = Agent

function sessionId(ref?: SessionRef): string {
  return ref?.id ?? `session-${crypto.randomUUID()}`
}

function statusFromDone(status: string): RunResult["status"] {
  if (status === "completed" || status === "done") return "completed"
  if (status === "cancelled" || status === "user" || status === "deadline" || status === "lease_lost" || status === "host_shutdown") return "cancelled"
  if (status === "failed" || status === "error") return "failed"
  return "partial"
}

const memoryOnlyProvider: LLMProvider = {
  async complete() {
    throw new Error("memory-only agent cannot perform a model completion")
  },
  async *stream() {
    throw new Error("memory-only agent cannot perform a model stream")
  },
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

class AgentRuntimeImpl implements Agent {
  readonly name: string
  readonly declaration: AgentDeclaration
  private readonly bindings: AgentHostBindings
  private readonly sessionLog: SessionLog
  private activeRunner: RuntimeRunner | null = null
  private mcpPlane?: McpProxyPlane
  private mcpConnection?: Promise<void>

  constructor(definition: AgentDefinition) {
    const captured = captureAgentDeclaration(definition)
    this.declaration = captured.declaration
    this.bindings = captured.bindings
    this.name = this.declaration.name
    this.sessionLog = this.bindings.runtimeBinding?.sessionLog ?? new InMemorySessionLog()
  }

  get definition(): Readonly<AgentDefinition> {
    return Object.freeze(materializeAgentDefinition(this.declaration, this.bindings))
  }

  session(id = `session-${crypto.randomUUID()}`): AgentSession {
    return new AgentSessionImpl(this, id)
  }

  async remember(input: MemoryInput): Promise<MemoryRecord> {
    const store = this.bindings.memoryStore
    const scope = this.bindings.memoryScope
    if (!store || !scope) throw new Error("agent memory requires memoryStore and memoryScope")
    const session = `memory-${crypto.randomUUID()}`
    const now = Date.now()
    const record: MemoryRecord = {
      record_id: crypto.randomUUID(),
      scope,
      name: input.name,
      kind: input.kind ?? "reference",
      content: input.content,
      description: input.description ?? input.name,
      provenance: { session_id: session, author: "host", trust: "user_asserted", evidence_refs: [] },
      created_at: now,
      updated_at: now,
      recall_count: 0,
      confidence: input.confidence ?? 1,
      links: [],
      pinned: input.pinned ?? false,
      ...(input.ttlDays !== undefined ? { ttl_days: input.ttlDays } : {}),
    }
    const runner = await this.createRunner({}, true)
    const accepted = await runner.writeMemory(record, { sessionId: session })
    if (!accepted) throw new Error(`agent "${this.name}" memory write denied`)
    return record
  }

  async recall(query: string, options: RecallOptions = {}): Promise<MemoryRecall[]> {
    const store = this.bindings.memoryStore
    const scope = this.bindings.memoryScope
    if (!store || !scope) throw new Error("agent memory requires memoryStore and memoryScope")
    const request: MemoryQuery = {
      scope,
      query,
      top_k: options.topK ?? 8,
      kinds: options.kinds ?? [],
      ...(options.minScore !== undefined ? { min_score: options.minScore } : {}),
    }
    const runner = await this.createRunner({}, true)
    return runner.queryMemory(request, { sessionId: `memory-${crypto.randomUUID()}` })
  }

  async delegate(request: DelegationRequest): Promise<DelegationResult> {
    const handoffs = this.declaration.handoffs ?? []
    if (handoffs.length) {
      if (!request.target) throw new Error(`agent "${this.name}" requires an explicit handoff target`)
      const targetName = agentRefName(request.target)
      const allowed = handoffs.some(handoff => {
        return agentRefName(handoff.agent) === targetName
      })
      if (!allowed) throw new Error(`agent "${this.name}" cannot hand off to "${targetName}"`)
    }
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
    const runner = await this.createRunner({})
    this.activeRunner = runner
    try {
      return await runner.runWorkflow(spec, { sessionId: sessionId(options.session) })
    } finally {
      this.activeRunner = null
    }
  }

  async listen(options: { session?: SessionRef; leaseMs?: number } = {}): Promise<RunResult | null> {
    const source = this.bindings.runtimeBinding?.runtimeOptions?.signalSource
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
    const owner = this
    return (async function* () {
      const runner = await owner.createRunner(options)
      owner.activeRunner = runner
      const abort = () => runner.interrupt("user")
      if (options.signal) {
        if (options.signal.aborted) runner.interrupt("user")
        else options.signal.addEventListener("abort", abort, { once: true })
      }
      const stream = runner.run({ sessionId: session, goal, ...(options.attachments?.length ? { attachments: options.attachments } : {}) })
      yield* owner.clearRunnerAfter(stream, options.signal, abort)
    })()
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
    const prepared = [...persisted].reverse().find(entry => entry.event.kind === "context_prepared")
    const measured = [...persisted].reverse().find(entry => entry.event.kind === "prompt_measured")
    const attempt = [...persisted].reverse().find(entry => entry.event.kind === "provider_attempt")
    const runStarted = [...persisted].reverse().find(entry => entry.event.kind === "run_started")
    const binding = this.bindings.runtimeBinding
    const evidence = {
      ...(prepared?.event.kind === "context_prepared" ? { contextBinding: prepared.event.preparation.binding } : {}),
      ...(attempt?.event.kind === "provider_attempt" ? { route: attempt.event.route } : runStarted?.event.kind === "run_started" && runStarted.event.route ? { route: runStarted.event.route } : {}),
      ...(measured?.event.kind === "prompt_measured" ? { measurement: measured.event.measurement } : {}),
      ...(binding?.runtimeOptions?.artifactSetDigest ? { artifactSet: { digest: binding.runtimeOptions.artifactSetDigest } } : {}),
    }
    const outputValidation = this.declaration.outputSchema
      ? validateAgainstSchema(extractJsonValue(output), materializeAgentDefinition(this.declaration, this.bindings).outputSchema!)
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
      ...(Object.keys(evidence).length ? { evidence } : {}),
    }
  }

  async *resume(id: string, options: Omit<AgentRunOptions, "session"> = {}): AsyncIterable<StreamEvent> {
    const runner = await this.createRunner(options)
    this.activeRunner = runner
    yield* this.clearRunnerAfter(runner.wake(id), options.signal, () => runner.interrupt("user"))
  }

  interrupt(reason: "user" | "deadline" | "lease_lost" | "host_shutdown" = "user"): void {
    this.activeRunner?.interrupt(reason)
  }

  async close(): Promise<void> {
    await this.mcpConnection
    await this.mcpPlane?.disconnect()
    this.mcpPlane = undefined
    this.mcpConnection = undefined
  }

  private async prepareMcp(): Promise<void> {
    if (!this.mcpPlane || this.mcpConnection) {
      await this.mcpConnection
      return
    }
    this.mcpConnection = this.mcpPlane.connect()
    await this.mcpConnection
  }

  private async createRunner(options: AgentRunOptions, allowUnboundProvider = false): Promise<RuntimeRunner> {
    const model = this.declaration.model
    const binding = this.bindings.runtimeBinding
    const provider = binding?.provider
      ?? (typeof model === "string" ? binding?.providerFor?.(model) : undefined)
    if (!provider && !allowUnboundProvider) {
      throw new Error(`agent "${this.name}" has no runtime provider binding for model ${typeof model === "string" ? model : "(unresolved)"}`)
    }
    if (binding?.executionPlane && this.declaration.mcpServers?.length) {
      throw new Error("agent mcpServers cannot be combined with a custom executionPlane")
    }
    const plane = binding?.executionPlane
      ?? (this.declaration.mcpServers?.length
        ? (() => {
            const servers = Object.fromEntries(this.declaration.mcpServers.map(server => {
              if (server.transport.kind !== "stdio") {
                throw new Error(`agent MCP transport "${server.transport.kind}" is not supported by the local runtime`)
              }
              if (server.auth && Object.keys(server.auth).length > 0) {
                throw new Error(`agent MCP server "${server.name ?? server.transport.command}" auth requires an explicit CredentialVault binding`)
              }
              return [server.name ?? server.transport.command, {
                command: server.transport.command,
                ...(server.transport.args ? { args: [...server.transport.args] } : {}),
              }]
            }))
            this.mcpPlane ??= new McpProxyPlane({ servers, vault: new EnvCredentialVault() })
            return this.mcpPlane
          })()
        : this.bindings.tools.reduce((current, currentTool) => current.register(currentTool), new LocalExecutionPlane()))
    if (this.declaration.mcpServers?.length && this.bindings.tools.length) {
      plane.register(...this.bindings.tools)
    }
    // MCP schemas are discovered during connect, before the adapter snapshots the baseline.
    await this.prepareMcp()
    return new RuntimeRunner(buildAgentRuntimeOptions(materializeAgentDefinition(this.declaration, this.bindings), options, {
      provider: provider ?? memoryOnlyProvider,
      executionPlane: plane,
      sessionLog: this.sessionLog,
      agentId: this.name,
    }))
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

export function createAgent(definition: AgentDefinition): Agent {
  return new AgentRuntimeImpl(definition)
}
