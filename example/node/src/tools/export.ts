import { tool } from "@deepstrike/sdk"
import { writeFile, mkdir } from "fs/promises"
import { join } from "path"
import { loadNotes } from "../archive.js"
import { OUTPUT_DIR } from "../paths.js"
import type { Note } from "../types.js"

export const exportTool = tool(
  "export",
  "Export notes as a formatted document (digest, outline, actions, or clusters)",
  {
    type: "object",
    properties: {
      format: {
        type: "string",
        enum: ["digest", "outline", "actions", "clusters"],
        description: "Output format",
      },
      filter: { type: "string", description: "Optional tag filter e.g. #machinelearning" },
    },
    required: ["format"],
  },
  async ({ format, filter }) => {
    const notes = await loadNotes()
    const fmt = String(format)
    const tag = filter ? String(filter) : undefined
    const filtered = tag ? notes.filter((n: Note) => n.tags.includes(tag)) : notes

    if (!filtered.length) return `No notes found${tag ? ` with tag ${tag}` : ""}.`

    const date = new Date().toISOString().slice(0, 10)
    const dir = join(OUTPUT_DIR, fmt === "clusters" ? "clusters" : "digest")
    await mkdir(dir, { recursive: true })

    let content = ""

    if (fmt === "digest") {
      const top = filtered.slice(0, 20)
      content = [
        `# FlashNote Digest — ${date}`,
        `${filtered.length} notes${tag ? ` · ${tag}` : ""}`,
        "",
        ...top.map(n =>
          `## ${n.summary}\n*${n.type}* · ${n.tags.join(" ")}\n\n${n.raw.slice(0, 300)}`
        ),
      ].join("\n\n")
    } else if (fmt === "outline") {
      const byType = new Map<string, Note[]>()
      for (const n of filtered) {
        const list = byType.get(n.type) ?? []
        list.push(n); byType.set(n.type, list)
      }
      content = `# FlashNote Outline — ${date}\n\n`
      for (const [type, items] of byType) {
        content += `## ${type}\n${items.map(n => `- ${n.summary}`).join("\n")}\n\n`
      }
    } else if (fmt === "actions") {
      const tasks = filtered.filter(n => n.type === "task")
      if (!tasks.length) return "No tasks found."
      content = `# FlashNote Actions — ${date}\n\n` +
        tasks.map(n => `- [ ] ${n.summary}`).join("\n")
    } else if (fmt === "clusters") {
      const byTag = new Map<string, Note[]>()
      for (const n of filtered) {
        const t = n.tags[0] ?? "#untagged"
        const list = byTag.get(t) ?? []
        list.push(n); byTag.set(t, list)
      }
      content = `# FlashNote Clusters — ${date}\n\n`
      for (const [t, items] of [...byTag.entries()].sort((a, b) => b[1].length - a[1].length)) {
        content += `## ${t} (${items.length})\n${items.map(n => `- ${n.summary}`).join("\n")}\n\n`
      }
    }

    const filename = join(dir, `${date}_${fmt}.md`)
    await writeFile(filename, content)

    const preview = content.slice(0, 600)
    return `Exported ${filtered.length} notes → ${filename}\n\n${preview}${content.length > 600 ? "\n…" : ""}`
  },
)
