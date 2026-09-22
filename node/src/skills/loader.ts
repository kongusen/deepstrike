import { readFile, readdir } from "fs/promises"
import path from "path"

const SAFE_SKILL_NAME = /^[A-Za-z0-9_-]+$/

function skillPath(skillDir: string, name: string): string {
  if (!SAFE_SKILL_NAME.test(name)) {
    throw new Error(`invalid skill name "${name}": use only letters, digits, "-", "_"`)
  }
  return path.join(skillDir, `${name}.md`)
}

export interface SkillMetadata {
  name: string
  description: string
  whenToUse?: string
  effort?: number
  estimatedTokens?: number
  /** Optional structured grants supplied by the SDK caller. They are not parsed from simple
   *  SKILL.md frontmatter: the canonical kernel validates their attenuation at activation. */
  capabilityGrants?: Array<Record<string, unknown>>
  /** P1-B tool gating: tool ids this skill needs. When the skill is active, the kernel narrows the
   *  exposed toolset to `stable-core ∪ allowedTools`. Parsed from `allowed_tools:` frontmatter
   *  (comma-separated or `[a, b]`). Absent ⇒ the skill does not narrow. */
  allowedTools?: string[]
  version?: string
  digest?: string
}

export type SkillResourceKind = "scripts" | "references" | "assets"
export interface SkillResourceRef { path: string; kind: SkillResourceKind; size?: number }
export interface SkillPackage {
  descriptor: SkillMetadata
  instructions: string
  resources: Record<SkillResourceKind, SkillResourceRef[]>
  root: string
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
  const file = skillPath(skillDir, name)
  try {
    const raw = await readFile(file, "utf8")
    return parseFrontmatter(raw).body
  } catch (error: unknown) {
    if ((error as { code?: string }).code !== "ENOENT") throw error
    const packageRaw = await readFile(path.join(skillDir, name, "SKILL.md"), "utf8").catch(() => null)
    return packageRaw === null ? null : parseFrontmatter(packageRaw).body
  }
}

/** Scan a skill directory and return frontmatter-only metadata for all `.md` files. */
export async function scanSkillDir(skillDir: string): Promise<SkillMetadata[]> {
  const files = await readdir(skillDir).catch(() => [] as string[])
  const results: SkillMetadata[] = []
  for (const name of files) {
    const stat = await import("fs/promises").then(fs => fs.stat(path.join(skillDir, name)).catch(() => null))
    if (!stat?.isDirectory()) continue
    const manifest = path.join(skillDir, name, "SKILL.md")
    const raw = await readFile(manifest, "utf8").catch(() => null)
    if (!raw) continue
    const { meta } = parseFrontmatter(raw)
    results.push({
      name: meta.name ? String(meta.name) : name,
      description: meta.description ? String(meta.description) : "",
      whenToUse: meta.when_to_use ? String(meta.when_to_use) : undefined,
      effort: meta.effort ? Number(meta.effort) : undefined,
      estimatedTokens: meta.estimated_tokens ? Number(meta.estimated_tokens) : undefined,
      allowedTools: parseToolList(meta.allowed_tools),
      version: meta.version ? String(meta.version) : undefined,
      digest: meta.digest ? String(meta.digest) : undefined,
    })
  }
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

function safeResourcePath(value: string): string {
  const normalized = path.posix.normalize(value.replaceAll("\\", "/"))
  if (normalized.startsWith("../") || normalized === ".." || path.posix.isAbsolute(normalized)) throw new Error("skill resource path escapes package")
  return normalized
}

/** Load a Claude-compatible package without reading optional resources into memory. */
export async function loadSkillPackage(skillDir: string, name: string): Promise<SkillPackage | null> {
  if (!SAFE_SKILL_NAME.test(name)) throw new Error(`invalid skill name "${name}"`)
  const root = path.join(skillDir, name)
  const raw = await readFile(path.join(root, "SKILL.md"), "utf8").catch(() => null)
  if (raw === null) return null
  const { meta, body } = parseFrontmatter(raw)
  const resources: Record<SkillResourceKind, SkillResourceRef[]> = { scripts: [], references: [], assets: [] }
  for (const kind of Object.keys(resources) as SkillResourceKind[]) {
    const directory = path.join(root, kind)
    const entries = await readdir(directory, { withFileTypes: true }).catch(() => [])
    for (const entry of entries) if (entry.isFile()) {
      const relative = safeResourcePath(path.join(kind, entry.name))
      const stat = await import("fs/promises").then(fs => fs.stat(path.join(root, relative)))
      resources[kind].push({ path: relative, kind, size: stat.size })
    }
  }
  return {
    descriptor: {
      name: meta.name ? String(meta.name) : name,
      description: meta.description ? String(meta.description) : "",
      whenToUse: meta.when_to_use ? String(meta.when_to_use) : undefined,
      effort: meta.effort ? Number(meta.effort) : undefined,
      estimatedTokens: meta.estimated_tokens ? Number(meta.estimated_tokens) : undefined,
      allowedTools: parseToolList(meta.allowed_tools),
      version: meta.version ? String(meta.version) : undefined,
      digest: meta.digest ? String(meta.digest) : undefined,
    },
    instructions: body,
    resources,
    root,
  }
}

export async function readSkillResource(pkg: SkillPackage, resource: SkillResourceRef): Promise<Uint8Array> {
  if (!pkg.resources[resource.kind].some(candidate => candidate.path === resource.path)) throw new Error("skill resource is not declared by the package")
  const bytes = await import("fs/promises").then(fs => fs.readFile(path.join(pkg.root, safeResourcePath(resource.path))))
  return new Uint8Array(bytes)
}
