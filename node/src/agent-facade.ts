import { Agent as DeclarativeAgent, type AgentOptions } from "./agent.js"
import { InMemorySessionLog, type SessionEvent, type SessionLog } from "./runtime/session-log.js"
import { LocalExecutionPlane, type ExecutionPlane } from "./runtime/execution-plane.js"
import { RuntimeRunner, type RuntimeOptions } from "./runtime/runner.js"
import type { LLMProvider, StreamEvent, DoneEvent, ErrorEvent, TokenUsage } from "./types.js"
import type { RegisteredTool } from "./tools/index.js"

export interface AgentDefinition extends Omit<AgentOptions, "model"> {
  provider: LLMProvider
  tools?: RegisteredTool[]
  executionPlane?: ExecutionPlane
  sessionLog?: SessionLog
  maxTokens?: number
}

export interface AgentRunOptions {
  session?: SessionRef
  maxTurns?: number
  signal?: AbortSignal
  metadata?: Record<string, unknown>
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

export interface ExecutableAgent {
  readonly name: string
  readonly definition: Readonly<AgentDefinition>
  run(goal: string, options?: AgentRunOptions): Promise<RunResult>
  stream(goal: string, options?: AgentRunOptions): AsyncIterable<StreamEvent>
  session(id?: string): AgentSession
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

  stream(goal: string, options: AgentRunOptions = {}): AsyncIterable<StreamEvent> {
    const session = sessionId(options.session)
    const runner = this.createRunner(options)
    this.activeRunner = runner
    const stream = runner.run({ sessionId: session, goal })
    return this.clearRunnerAfter(stream)
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
    yield* this.clearRunnerAfter(runner.wake(id))
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
    }
    return new RuntimeRunner(runtime)
  }

  private async *clearRunnerAfter(stream: AsyncIterable<StreamEvent>): AsyncIterable<StreamEvent> {
    try {
      yield* stream
    } finally {
      this.activeRunner = null
    }
  }
}

export function createAgent(definition: AgentDefinition): ExecutableAgent {
  return new ExecutableAgentImpl(definition)
}
