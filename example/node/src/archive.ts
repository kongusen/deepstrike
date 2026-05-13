import { readdir, readFile, writeFile, mkdir } from "fs/promises"
import { join } from "path"
import { ARCHIVE_DIR } from "./paths.js"
import type { Note } from "./types.js"

export function generateId(): string {
  const now = new Date()
  const date = now.toISOString().slice(0, 10).replace(/-/g, "")
  const time = now.toTimeString().slice(0, 8).replace(/:/g, "")
  const rand = Math.random().toString(36).slice(2, 8)
  return `${date}_${time}_${rand}`
}

export async function saveNote(note: Note): Promise<void> {
  await mkdir(ARCHIVE_DIR, { recursive: true })
  await writeFile(join(ARCHIVE_DIR, `${note.id}.json`), JSON.stringify(note, null, 2))
}

export async function loadNotes(limit = 200): Promise<Note[]> {
  try {
    const files = (await readdir(ARCHIVE_DIR))
      .filter(f => f.endsWith(".json"))
      .slice(-limit)
    const notes: Note[] = []
    for (const f of files) {
      try {
        const raw = await readFile(join(ARCHIVE_DIR, f), "utf8")
        notes.push(JSON.parse(raw) as Note)
      } catch { /* skip malformed */ }
    }
    return notes.sort((a, b) => b.createdAt - a.createdAt)
  } catch {
    return []
  }
}

/** Parse agent output: extract first JSON block, return Note fields or null */
export function parseNoteOutput(text: string, raw: string, source: Note["source"], contributor?: string): Note | null {
  const match = text.match(/```json\s*([\s\S]*?)```/) ?? text.match(/\{[\s\S]*"type"[\s\S]*\}/)
  const jsonStr = match ? (match[1] ?? match[0]) : text.trim()
  try {
    const parsed = JSON.parse(jsonStr) as Partial<Note>
    if (!parsed.type || !parsed.tags || !parsed.summary) return null
    return {
      id: generateId(),
      type: parsed.type,
      tags: parsed.tags,
      summary: parsed.summary,
      connections: parsed.connections ?? [],
      source,
      contributor,
      raw,
      url: parsed.url,
      qualityScore: 0.8,
      createdAt: Date.now(),
    }
  } catch {
    return null
  }
}
