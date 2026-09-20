import { type AgentOptions } from "./agent.js"
import { InMemorySessionLog, type SessionLog } from "./runtime/session-log.js"
import { LocalExecutionPlane, type ExecutionPlane } from "./runtime/execution-plane.js"
import { RuntimeRunner, type RuntimeOptions } from "./runtime/runner.js"
import type { LLMProvider, StreamEvent, DoneEvent, ErrorEvent, TokenUsage } from "./types.js"
import type { RegisteredTool } from "./tools/index.js"
import type { MemoryRecord, MemoryRecall, MemoryQuery, MemoryScope, MemoryStore, MemoryKind } from "./memory/protocols.js"
import type { WorkflowSpec, WorkflowOutcome, KernelAgentRole } from "./types/agent.js"

export interface AgentDefinition extends Omit<AgentOptions, "model" | "name"> {
  name?: string
  provider: LLMProvider
  tools?: RegisteredTool[]
  executionPlane?: ExecutionPlane
  sessionLog?: SessionLog
  maxTokens?: number
  memoryStore?: MemoryStore
  memoryScope?: MemoryScope
  runtimeOptions?: Pick<RuntimeOptions, "memoryPolicy" | "governancePolicy" | "signalSource" | "signalPolicy" | "resourceQuota" | "onPermissionRequest" | "payloadStore" | "runGroup" | "subAgentOrchestrator" | "reducers">
}

export interface AgentRunOptions {
  session?: SessionRef
  maxTurns?: number
  signal?: AbortSignal
  metadata?: Record<string, unknown>
  onPermissionRequest?: RuntimeOptions["onPermissionRequest"]
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
  instructions?: string
  tools?: RegisteredTool[]
}

export interface DelegationResult {
  output: string
  status: "completed" | "partial" | "failed"
  nodeId?: string
}

export interface ExecutableAgent {
  readonly name: string
  readonly definition: Readonly<AgentDefinition>
  run(goal: string, options?: AgentRunOptions): Promise<RunResult>
  stream(goal: string, options?: AgentRunOptions): AsyncIterable<StreamEvent>
  session(id?: string): AgentSession
  remember(input: MemoryInput): Promise<MemoryRecord>
  recall(query: string, options?: RecallOptions): Promise<MemoryRecall[]>
  delegate(request: DelegationRequest): Promise<DelegationResult>
  workflow(spec: WorkflowSpec, options?: { session?: SessionRef }): Promise<WorkflowOutcome>
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

class AgentSessionImpl implements AgentSession {
  constructor(private readonly owner: ExecutableAgentImpl, public readonly id: string) {}

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

class ExecutableAgentImpl implements ExecutableAgent {
  readonly name: string
  readonly definition: Readonly<AgentDefinition>
  private readonly sessionLog: SessionLog
  private activeRunner: RuntimeRunner | null = null

  constructor(definition: AgentDefinition) {
    if (!definition.provider) throw new TypeError("createAgent requires a provider")
    this.definition = Object.freeze({ ...definition })
    this.name = definition.name ?? "agent"
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

  stream(goal: string, options: AgentRunOptions = {}): AsyncIterable<StreamEvent> {
    const session = sessionId(options.session)
    const runner = this.createRunner(options)
    this.activeRunner = runner
    const abort = () => runner.interrupt("user")
    if (options.signal) {
      if (options.signal.aborted) runner.interrupt("user")
      else options.signal.addEventListener("abort", abort, { once: true })
    }
    const stream = runner.run({ sessionId: session, goal })
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
    return {
      output,
      runId: started?.event.kind === "run_started" ? started.event.run_id : `run-${crypto.randomUUID()}`,
      sessionId: session,
      status: error ? "failed" : statusFromDone(done?.status ?? "partial"),
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
    const plane = this.definition.executionPlane
      ?? (this.definition.tools ?? []).reduce((current, currentTool) => current.register(currentTool), new LocalExecutionPlane())
    const runtime: RuntimeOptions = {
      provider: this.definition.provider,
      executionPlane: plane,
      sessionLog: this.sessionLog,
      maxTokens: this.definition.maxTokens ?? 32_000,
      ...(this.definition.instructions ? { systemPrompt: this.definition.instructions } : {}),
      ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
      ...(this.definition.memoryStore ? { memoryStore: this.definition.memoryStore } : {}),
      ...(this.definition.memoryScope ? { memoryScope: this.definition.memoryScope } : {}),
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

export function createAgent(definition: AgentDefinition): ExecutableAgent {
  return new ExecutableAgentImpl(definition)
}
