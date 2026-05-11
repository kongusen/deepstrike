import { readFile, readdir } from "fs/promises"
import path from "path"

export interface SkillMetadata {
  name: string
  description: string
  whenToUse?: string
  effort?: number
  estimatedTokens?: number
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

/** Read one skill file and return its full markdown content. */
export async function readSkillFile(skillDir: string, name: string): Promise<string | null> {
  try {
    return await readFile(path.join(skillDir, `${name}.md`), "utf8")
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
    const content = await readSkillFile(skillDir, name)
    if (!content) continue
    const { meta } = parseFrontmatter(content)
    results.push({
      name: meta.name ? String(meta.name) : name,
      description: meta.description ? String(meta.description) : "",
      whenToUse: meta.when_to_use ? String(meta.when_to_use) : undefined,
      effort: meta.effort ? Number(meta.effort) : undefined,
      estimatedTokens: meta.estimated_tokens ? Number(meta.estimated_tokens) : undefined,
    })
  }
  return results
}
