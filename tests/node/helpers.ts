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
import type {
  DreamStore,
  KnowledgeSource,
  MemoryQuery,
  MemoryRecall,
  MemoryRecord,
  MemoryScope,
  SessionData,
} from "@deepstrike/sdk/memory"
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
  memoryScope?: MemoryScope
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
    memoryScope: options.memoryScope,
    knowledgeSource: options.knowledgeSource,
    governance: options.governance,
  })
  return new RunnerHandle(runner, plane)
}

/** Test-only alias for `makeRunner` (not the removed Agent class). */
export const makeAgent = makeRunner

export const SKILL_DIR = resolve(__dirname, "fixtures/skills")

export const TEST_MEMORY_SCOPE: MemoryScope = {
  tenant_id: "tests",
  namespace: "node-sdk",
}

export function memoryRecord(
  name: string,
  content: string,
  confidence = 0.5,
  scope: MemoryScope = TEST_MEMORY_SCOPE,
): MemoryRecord {
  const now = Date.now()
  return {
    record_id: `fixture-${name}`,
    scope,
    name,
    kind: "reference",
    content,
    description: content,
    provenance: {
      author: "host",
      trust: "host_verified",
      evidence_refs: [],
    },
    created_at: now,
    updated_at: now,
    recall_count: 0,
    confidence,
    links: [],
    pinned: false,
  }
}

export class MockDreamStore implements DreamStore {
  private memories = new Map<string, MemoryRecord[]>()
  savedSessions: SessionData[] = []

  async upsert(agentId: string, incoming: MemoryRecord): Promise<void> {
    const kept = [...(this.memories.get(agentId) ?? [])]
    const index = kept.findIndex(record => record.scope.tenant_id === incoming.scope.tenant_id
      && record.scope.namespace === incoming.scope.namespace
      && record.kind === incoming.kind && record.name === incoming.name)
    if (index >= 0) kept[index] = incoming
    else kept.push(incoming)
    this.memories.set(agentId, kept)
  }

  async search(agentId: string, query: MemoryQuery): Promise<MemoryRecall[]> {
    return (this.memories.get(agentId) ?? [])
      .filter(record => record.scope.tenant_id === query.scope.tenant_id && record.scope.namespace === query.scope.namespace)
      .slice(0, query.top_k)
      .map(record => ({ record, score: record.confidence, why: "fixture" }))
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
