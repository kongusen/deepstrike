import { tool } from "@deepstrike/sdk"
import { loadNotes } from "../archive.js"
import type { Note } from "../types.js"

export const searchArchive = tool(
  "search_archive",
  "Search the note archive for related notes by keyword",
  {
    type: "object",
    properties: {
      query: { type: "string", description: "Keywords to search for" },
      topK: { type: "number", description: "Max results (default 5)" },
      source: { type: "string", description: "Filter: personal | community | (omit for all)" },
    },
    required: ["query"],
  },
  async ({ query, topK = 5, source }) => {
    const notes = await loadNotes()
    const q = String(query).toLowerCase()
    const terms = q.split(/\s+/).filter(Boolean)

    const scored = notes
      .filter((n: Note) => source ? n.source === String(source) : true)
      .map((n: Note) => {
        const haystack = `${n.summary} ${n.tags.join(" ")} ${n.raw}`.toLowerCase()
        const hits = terms.reduce((acc, t) => acc + (haystack.match(new RegExp(t, "g"))?.length ?? 0), 0)
        return { note: n, hits }
      })
      .filter(({ hits }) => hits > 0)
      .sort((a, b) => b.hits - a.hits)
      .slice(0, Number(topK))

    if (!scored.length) return "No related notes found in archive."

    return scored
      .map(({ note: n }) => `[${n.id}] ${n.summary}\n  tags: ${n.tags.join(" ")} | type: ${n.type}`)
      .join("\n\n")
  },
)
