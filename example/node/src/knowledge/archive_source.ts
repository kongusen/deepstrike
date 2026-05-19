import { loadNotes } from "../archive.js"
import type { KnowledgeSource } from "@deepstrike/sdk"

export class ArchiveKnowledgeSource implements KnowledgeSource {
  async init(): Promise<void> {}

  async retrieve(goal: string, topK = 5): Promise<string[]> {
    const notes = await loadNotes()
    if (!notes.length) return []

    const terms = goal.toLowerCase().split(/\s+/).filter(t => t.length > 2)
    const scored = notes.map(n => {
      const haystack = `${n.summary} ${n.tags.join(" ")} ${n.raw.slice(0, 500)}`.toLowerCase()
      const hits = terms.reduce((acc, t) => acc + (haystack.includes(t) ? 1 : 0), 0)
      return { n, hits }
    })
    .filter(({ hits }) => hits > 0)
    .sort((a, b) => b.hits - a.hits)
    .slice(0, topK)

    return scored.map(({ n }) =>
      `[${n.id}] ${n.summary}\ntags: ${n.tags.join(" ")} | type: ${n.type}\n${n.raw.slice(0, 200)}`
    )
  }
}

export function makeArchiveSource(): ArchiveKnowledgeSource {
  return new ArchiveKnowledgeSource()
}
