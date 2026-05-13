import { readFile, writeFile, mkdir, readdir } from "fs/promises"
import { join } from "path"
import { MEMORY_DIR } from "../paths.js"
import type { DreamStore, SessionData, MemoryEntry, CurationResult } from "@deepstrike/sdk"

function agentDir(agentId: string) {
  return join(MEMORY_DIR, agentId)
}

export class FileDreamStore implements DreamStore {
  async loadSessions(agentId: string): Promise<SessionData[]> {
    const dir = join(agentDir(agentId), "sessions")
    try {
      const files = (await readdir(dir)).filter(f => f.endsWith(".json"))
      const sessions: SessionData[] = []
      for (const f of files) {
        try {
          const raw = await readFile(join(dir, f), "utf8")
          sessions.push(JSON.parse(raw) as SessionData)
        } catch { /* skip malformed */ }
      }
      return sessions.sort((a, b) => a.createdAtMs - b.createdAtMs)
    } catch {
      return []
    }
  }

  async saveSession(agentId: string, session: SessionData): Promise<void> {
    const dir = join(agentDir(agentId), "sessions")
    await mkdir(dir, { recursive: true })
    await writeFile(join(dir, `${session.sessionId}.json`), JSON.stringify(session, null, 2))
  }

  async loadMemories(agentId: string): Promise<MemoryEntry[]> {
    const path = join(agentDir(agentId), "memories.json")
    try {
      const raw = await readFile(path, "utf8")
      return JSON.parse(raw) as MemoryEntry[]
    } catch {
      return []
    }
  }

  async commit(agentId: string, result: CurationResult, existing: MemoryEntry[]): Promise<void> {
    const kept = existing.filter((_, i) => !result.toRemoveIndices.includes(i))
    const updated = [...kept, ...result.toAdd]
    const dir = agentDir(agentId)
    await mkdir(dir, { recursive: true })
    await writeFile(join(dir, "memories.json"), JSON.stringify(updated, null, 2))
  }

  async search(agentId: string, query: string, topK = 5): Promise<MemoryEntry[]> {
    const memories = await this.loadMemories(agentId)
    if (!memories.length) return []
    const q = query.toLowerCase()
    const terms = q.split(/\s+/).filter(Boolean)
    return memories
      .map(m => {
        const hits = terms.reduce((acc, t) => acc + (m.text.toLowerCase().includes(t) ? 1 : 0), 0)
        return { ...m, _hits: hits }
      })
      .filter(m => m._hits > 0)
      .sort((a, b) => b._hits - a._hits || b.score - a.score)
      .slice(0, topK)
      .map(({ _hits: _, ...m }) => m)
  }
}

export function makeFileDreamStore(): FileDreamStore {
  return new FileDreamStore()
}
