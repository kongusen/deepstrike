import { createRequire } from "module"
import { resolve, dirname } from "path"
import { fileURLToPath } from "url"

const __dirname = dirname(fileURLToPath(import.meta.url))
const dotenv = createRequire(import.meta.url)("dotenv") as { config(opts: { path: string }): void }
dotenv.config({ path: resolve(__dirname, "../../.env") })

import {
  OpenAIProvider, Agent,
  type AgentOptions, type StreamEvent,
} from "@deepstrike/sdk"
import type { DreamStore, SessionData, MemoryEntry, CurationResult } from "@deepstrike/sdk"
import type { KnowledgeSource } from "@deepstrike/sdk"

export const ENV = {
  apiKey:  process.env.OPENAI_API_KEY  ?? "",
  model:   process.env.OPENAI_MODEL    ?? "gpt-4o-mini",
  baseURL: process.env.OPENAI_BASE_URL ?? "https://api.openai.com/v1",
}

export function makeProvider() {
  return new OpenAIProvider(ENV.apiKey, ENV.model, { maxRetries: 2, baseDelay: 500 }, ENV.baseURL)
}

export function makeAgent(options: Partial<AgentOptions> = {}) {
  return new Agent(makeProvider(), { maxTokens: 4096, maxTurns: 10, ...options })
}

export const SKILL_DIR = resolve(__dirname, "fixtures/skills")

// ─── In-memory DreamStore ──────────────────────────────────────────────────

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

// ─── In-memory KnowledgeSource ─────────────────────────────────────────────

export class MockKnowledgeSource implements KnowledgeSource {
  initCalled = 0
  constructor(private readonly snippets: string[]) {}
  async init(): Promise<void> { this.initCalled++ }
  async retrieve(_query: string, topK = 5): Promise<string[]> {
    return this.snippets.slice(0, topK)
  }
}

// ─── Stream helpers ─────────────────────────────────────────────────────────

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
