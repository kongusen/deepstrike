import { createRequire } from "module"
import { resolve, dirname } from "path"
import { fileURLToPath } from "url"

const __dirname = dirname(fileURLToPath(import.meta.url))
const dotenv = createRequire(import.meta.url)("dotenv") as { config(opts: { path: string }): void }
dotenv.config({ path: resolve(__dirname, "../../.env") })

import {
  OpenAIProvider,
  RuntimeRunner,
  collectText,
  InMemorySessionLog,
  LocalExecutionPlane,
  type StreamEvent,
} from "@deepstrike/sdk"
import type { DreamStore, SessionData, MemoryEntry, CurationResult } from "@deepstrike/sdk"
import type { KnowledgeSource } from "@deepstrike/sdk"
import type { RegisteredTool } from "@deepstrike/sdk"
import type { Governance } from "@deepstrike/sdk"

export const ENV = {
  apiKey:  process.env.OPENAI_API_KEY  ?? "",
  model:   process.env.OPENAI_MODEL    ?? "gpt-4o-mini",
  baseURL: process.env.OPENAI_BASE_URL ?? "https://api.openai.com/v1",
}

export function makeProvider() {
  return new OpenAIProvider(ENV.apiKey, ENV.model, { maxRetries: 2, baseDelay: 500 }, ENV.baseURL)
}

export interface MakeRunnerOptions {
  maxTokens?: number
  maxTurns?: number
  systemPrompt?: string
  initialMemory?: string[]
  skillDir?: string
  dreamStore?: DreamStore
  agentId?: string
  knowledgeSource?: KnowledgeSource
  governance?: Governance
  tools?: RegisteredTool[]
}

export class RunnerHandle {
  constructor(
    readonly runner: RuntimeRunner,
    private readonly plane: LocalExecutionPlane,
  ) {}

  register(...tools: RegisteredTool[]): this {
    for (const t of tools) this.plane.register(t)
    return this
  }

  interrupt(): void {
    this.runner.interrupt()
  }

  runStreaming(goal: string, opts: { criteria?: string[]; sessionId?: string } = {}) {
    return this.runner.run({
      sessionId: opts.sessionId ?? crypto.randomUUID(),
      goal,
      criteria: opts.criteria,
    })
  }

  async run(goal: string, opts: { criteria?: string[]; sessionId?: string } = {}): Promise<string> {
    return collectText(this.runStreaming(goal, opts))
  }

  async dream(agentId: string, nowMs?: number) {
    return this.runner.dream(agentId, nowMs)
  }
}

export function makeRunner(options: MakeRunnerOptions = {}): RunnerHandle {
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  for (const t of options.tools ?? []) plane.register(t)
  const runner = new RuntimeRunner({
    provider: makeProvider(),
    sessionLog,
    executionPlane: plane,
    maxTokens: options.maxTokens ?? 4096,
    maxTurns: options.maxTurns ?? 10,
    systemPrompt: options.systemPrompt,
    initialMemory: options.initialMemory,
    skillDir: options.skillDir,
    dreamStore: options.dreamStore,
    agentId: options.agentId,
    knowledgeSource: options.knowledgeSource,
    governance: options.governance,
  })
  return new RunnerHandle(runner, plane)
}

/** Test-only alias for `makeRunner` (not the removed Agent class). */
export const makeAgent = makeRunner

export const SKILL_DIR = resolve(__dirname, "fixtures/skills")

export class MockDreamStore implements DreamStore {
  private sessions = new Map<string, SessionData[]>()
  private memories = new Map<string, MemoryEntry[]>()
  savedSessions: SessionData[] = []

  addSession(agentId: string, session: SessionData): void {
    const list = this.sessions.get(agentId) ?? []
    list.push(session)
    this.sessions.set(agentId, list)
  }

  async loadSessions(agentId: string): Promise<SessionData[]> {
    return this.sessions.get(agentId) ?? []
  }

  async loadMemories(agentId: string): Promise<MemoryEntry[]> {
    return this.memories.get(agentId) ?? []
  }

  async commit(agentId: string, result: CurationResult, existing: MemoryEntry[]): Promise<void> {
    const kept = existing.filter((_, i) => !result.toRemoveIndices.includes(i))
    this.memories.set(agentId, [...kept, ...result.toAdd])
  }

  async search(agentId: string, _query: string, topK = 5): Promise<MemoryEntry[]> {
    return (this.memories.get(agentId) ?? []).slice(0, topK)
  }

  async saveSession(data: SessionData): Promise<void> {
    this.savedSessions.push(data)
  }
}

export class MockKnowledgeSource implements KnowledgeSource {
  initCalled = 0
  constructor(private readonly snippets: string[]) {}
  async init(): Promise<void> { this.initCalled++ }
  async retrieve(_query: string, topK = 5): Promise<string[]> {
    return this.snippets.slice(0, topK)
  }
}

export async function collectEvents(gen: AsyncIterable<StreamEvent>): Promise<StreamEvent[]> {
  const events: StreamEvent[] = []
  for await (const e of gen) events.push(e)
  return events
}

export function text(events: StreamEvent[]): string {
  return events
    .filter(e => e.type === "text_delta")
    .map(e => (e as { delta: string }).delta)
    .join("")
}
