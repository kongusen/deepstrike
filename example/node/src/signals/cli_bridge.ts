export type Command =
  | { type: "dump"; text: string }
  | { type: "research"; topic: string }
  | { type: "interview"; contributor: string; topic: string }
  | { type: "export"; format: string }
  | { type: "export_dataset"; format: string; minQuality: number; anonymize: boolean }
  | { type: "cluster"; topic: string }
  | { type: "stop" }
  | { type: "help" }
  | { type: "unknown"; raw: string }

export function parseCommand(line: string): Command {
  const t = line.trim()
  if (!t || t === "/help") return { type: "help" }
  if (t === "/stop") return { type: "stop" }

  if (t.startsWith("/dump ")) {
    const text = t.slice(6).trim()
    return text ? { type: "dump", text } : { type: "unknown", raw: t }
  }

  if (t.startsWith("/research ")) {
    const topic = t.slice(10).trim()
    return topic ? { type: "research", topic } : { type: "unknown", raw: t }
  }

  if (t.startsWith("/interview")) {
    const rest = t.slice(10).trim()
    const parts = rest.split(/\s+/, 2)
    return {
      type: "interview",
      contributor: parts[0] ?? "anonymous",
      topic: parts.slice(1).join(" ") || "general",
    }
  }

  if (t.startsWith("/export-dataset")) {
    const rest = t.slice(15).trim().split(/\s+/)
    const format = ["jsonl", "rag", "memory_pack"].includes(rest[0]) ? rest[0] : "jsonl"
    const minQuality = parseFloat(rest.find(r => /^0\.\d+$/.test(r)) ?? "0.7")
    const anonymize = rest.includes("--anonymize")
    return { type: "export_dataset", format, minQuality, anonymize }
  }

  if (t.startsWith("/export")) {
    const rest = t.slice(7).trim()
    const fmt = ["digest", "outline", "actions", "clusters"].includes(rest) ? rest : "digest"
    return { type: "export", format: fmt }
  }

  if (t.startsWith("/cluster ")) {
    const topic = t.slice(9).trim()
    return topic ? { type: "cluster", topic } : { type: "unknown", raw: t }
  }

  return { type: "unknown", raw: t }
}

export const HELP_TEXT = `
FlashNote commands:
  /dump <text>                        capture a thought or note
  /research <topic>                   deep research on a topic
  /interview [contributor] [topic]    start structured interview capture
  /export [digest|outline|actions|clusters]   export notes (default: digest)
  /export-dataset [jsonl|rag|memory_pack] [--anonymize]   export ML dataset
  /cluster <topic>                    cluster and synthesize notes on a topic
  /stop                               save memory and exit
`.trim()
