import { tool } from "@deepstrike/sdk"
import { writeFile, mkdir } from "fs/promises"
import { join } from "path"
import { loadNotes } from "../archive.js"
import { OUTPUT_DIR } from "../paths.js"
import type { Note } from "../types.js"

const PII_PATTERNS = [
  /\b\d{3}-\d{3}-\d{4}\b/g,
  /\b\d{11}\b/g,
  /\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b/g,
  /\b1[3-9]\d{9}\b/g,
]

function redact(text: string): string {
  let out = text
  for (const p of PII_PATTERNS) out = out.replace(p, "[REDACTED]")
  return out
}

export const exportDataset = tool(
  "export_dataset",
  "Export the archive as an ML-ready dataset in JSONL, RAG corpus, or memory_pack format",
  {
    type: "object",
    properties: {
      format: {
        type: "string",
        enum: ["jsonl", "rag", "memory_pack"],
        description: "Output format",
      },
      min_quality: { type: "number", description: "Minimum quality score 0–1 (default 0.7)" },
      anonymize: { type: "boolean", description: "Remove contributor info and redact PII" },
    },
    required: ["format"],
  },
  async ({ format, min_quality = 0.7, anonymize = false }) => {
    const notes = await loadNotes()
    const minQ = Number(min_quality)
    const anon = Boolean(anonymize)
    const fmt = String(format)

    const filtered = notes.filter((n: Note) => n.qualityScore >= minQ)
    if (!filtered.length) return `No notes meet quality threshold ${minQ}.`

    const clean = filtered.map((n: Note) => ({
      ...n,
      raw: anon ? redact(n.raw) : n.raw,
      summary: anon ? redact(n.summary) : n.summary,
      contributor: anon ? undefined : n.contributor,
    }))

    const date = new Date().toISOString().slice(0, 10)
    const dir = join(OUTPUT_DIR, "datasets")
    await mkdir(dir, { recursive: true })

    let content = ""
    let filename = ""

    if (fmt === "jsonl") {
      content = clean.map(n => JSON.stringify({
        text: `${n.summary}\n\n${n.raw}`,
        metadata: { type: n.type, tags: n.tags, id: n.id, source: n.source },
      })).join("\n")
      filename = join(dir, `${date}_notes.jsonl`)
    } else if (fmt === "rag") {
      content = clean.map(n => JSON.stringify({
        id: n.id,
        content: n.raw,
        metadata: { summary: n.summary, tags: n.tags, type: n.type, score: n.qualityScore },
      })).join("\n")
      filename = join(dir, `${date}_rag_corpus.jsonl`)
    } else {
      content = JSON.stringify(clean.map(n => ({
        text: n.summary,
        score: n.qualityScore,
        metadata: { type: n.type, tags: n.tags, id: n.id },
      })), null, 2)
      filename = join(dir, `${date}_memory_pack.json`)
    }

    await writeFile(filename, content)
    const size = Buffer.byteLength(content)
    return `Exported ${clean.length} notes (quality ≥ ${minQ}${anon ? ", anonymized" : ""}) → ${filename} (${(size / 1024).toFixed(1)} KB)`
  },
)
