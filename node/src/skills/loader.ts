import { readFile, readdir } from "fs/promises"
import path from "path"

export interface SkillMetadata {
  name: string
  description: string
  whenToUse?: string
  effort?: number
  estimatedTokens?: number
  /** P1-B tool gating: tool ids this skill needs. When the skill is active, the kernel narrows the
   *  exposed toolset to `stable-core ∪ allowedTools`. Parsed from `allowed_tools:` frontmatter
   *  (comma-separated or `[a, b]`). Absent ⇒ the skill does not narrow (back-compat). */
  allowedTools?: string[]
}

/** Parse a frontmatter tool list: `read, write` or `[read, write]` → ["read","write"]. */
function parseToolList(v: unknown): string[] | undefined {
  if (v == null || v === "") return undefined
  const ids = String(v).trim().replace(/^\[|\]$/g, "").split(",")
    .map(x => x.trim().replace(/^["']|["']$/g, "")).filter(Boolean)
  return ids.length ? ids : undefined
}

function parseFrontmatter(content: string): { meta: Record<string, unknown>; body: string } {
  const match = content.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/)
  if (!match) return { meta: {}, body: content }
  const meta: Record<string, unknown> = {}
  for (const line of match[1].split("\n")) {
    const [k, ...v] = line.split(":")
    if (k && v.length) meta[k.trim()] = v.join(":").trim()
  }
  return { meta, body: match[2] }
}

/** Read one skill file and return its body (frontmatter stripped). */
export async function readSkillFile(skillDir: string, name: string): Promise<string | null> {
  try {
    const raw = await readFile(path.join(skillDir, `${name}.md`), "utf8")
    return parseFrontmatter(raw).body
  } catch {
    return null
  }
}

/** Scan a skill directory and return frontmatter-only metadata for all `.md` files. */
export async function scanSkillDir(skillDir: string): Promise<SkillMetadata[]> {
  const files = await readdir(skillDir).catch(() => [] as string[])
  const results: SkillMetadata[] = []
  for (const file of files.filter(f => f.endsWith(".md"))) {
    const name = file.slice(0, -3)
    const raw = await readFile(path.join(skillDir, `${name}.md`), "utf8").catch(() => null)
    if (!raw) continue
    const { meta } = parseFrontmatter(raw)
    results.push({
      name: meta.name ? String(meta.name) : name,
      description: meta.description ? String(meta.description) : "",
      whenToUse: meta.when_to_use ? String(meta.when_to_use) : undefined,
      effort: meta.effort ? Number(meta.effort) : undefined,
      estimatedTokens: meta.estimated_tokens ? Number(meta.estimated_tokens) : undefined,
      allowedTools: parseToolList(meta.allowed_tools),
    })
  }
  return results
}
