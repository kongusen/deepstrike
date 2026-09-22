import { readSkillFile, scanSkillDir, type SkillMetadata } from "./skills/loader.js"
import path from "node:path"

export interface SkillResource { name: string; uri?: string; content?: string; metadata?: Record<string, unknown> }
export interface SkillScript { name: string; command: string; description?: string; metadata?: Record<string, unknown> }
export type SkillTool = string | { name: string; description?: string; inputSchema?: Record<string, unknown> }
export type SkillMCPServer = string | { name: string; transport: Record<string, unknown> }
export type SkillKnowledge = string | { name: string; content?: string; source?: unknown }

/** spc_001 §2.4: public Skill contract, built directly on `SKILL.md`-style frontmatter files. */
export interface Skill {
  name: string
  description?: string
  instructions?: string
  resources?: SkillResource[]
  scripts?: SkillScript[]
  tools?: SkillTool[]
  mcpServers?: SkillMCPServer[]
  knowledge?: SkillKnowledge[]
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

/** A source-independent public reference. The runtime resolves it inside a user/tenant scope. */
export interface SkillRef {
  name: string
  version?: string
  digest?: string
}

export type SkillDeclaration = Skill | SkillRef

/** Named L1 → L2 crossing for inline declarations. Directory/database sources converge here. */
export function projectSkillMetadata(skill: Skill): SkillMetadata {
  return {
    name: skill.name,
    description: skill.description ?? "",
    ...(skill.metadata?.whenToUse !== undefined ? { whenToUse: String(skill.metadata.whenToUse) } : {}),
  }
}

export interface SkillLoadContext {
  userId: string
  tenantId?: string
  namespace?: string
}

/** L2 resolver shared by inline, directory, database, and remote skill sources. */
export interface SkillCatalog {
  list(context: SkillLoadContext): Promise<SkillRef[]>
  resolve(ref: SkillRef, context: SkillLoadContext): Promise<Skill>
}

export class InlineSkillCatalog implements SkillCatalog {
  constructor(private readonly skills: readonly Skill[]) {}

  async list(_context: SkillLoadContext): Promise<SkillRef[]> {
    return this.skills.map(skill => ({ name: skill.name }))
  }

  async resolve(ref: SkillRef, _context: SkillLoadContext): Promise<Skill> {
    const skill = this.skills.find(candidate => candidate.name === ref.name)
    if (!skill) throw new Error(`skill "${ref.name}" is not available in the inline catalog`)
    return structuredClone(skill)
  }
}

export class DirectorySkillCatalog implements SkillCatalog {
  constructor(private readonly root: string) {}

  private scopedRoot(context: SkillLoadContext): string {
    for (const value of [context.userId, context.tenantId, context.namespace]) {
      if (value !== undefined && !/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("skill catalog scope contains an unsafe segment")
    }
    return path.join(this.root, context.tenantId ?? "default", context.userId, ...(context.namespace ? [context.namespace] : []))
  }

  async list(context: SkillLoadContext): Promise<SkillRef[]> {
    return (await scanSkillDir(this.scopedRoot(context))).map(skill => ({ name: skill.name }))
  }

  async resolve(ref: SkillRef, context: SkillLoadContext): Promise<Skill> {
    const skill = await loadSkill(this.scopedRoot(context), ref.name)
    if (!skill) throw new Error(`skill "${ref.name}" was not found for user "${context.userId}"`)
    return skill
  }
}

/** Adapter for database-backed or remote catalogs. The storage implementation remains host-owned. */
export class ResolverSkillCatalog implements SkillCatalog {
  constructor(
    private readonly resolver: (ref: SkillRef, context: SkillLoadContext) => Promise<Skill>,
    private readonly lister: (context: SkillLoadContext) => Promise<SkillRef[]>,
  ) {}

  list(context: SkillLoadContext): Promise<SkillRef[]> { return this.lister(context) }
  resolve(ref: SkillRef, context: SkillLoadContext): Promise<Skill> { return this.resolver(ref, context) }
}

/** Database/remote adapter with the same L1 contract as DirectorySkillCatalog. */
export class DatabaseSkillCatalog extends ResolverSkillCatalog {}

/** Loads one skill by name from a skill directory, reusing the existing frontmatter scanner and
 *  body reader — does not reimplement directory scanning. Returns `null` if the skill file is
 *  absent (mirrors `readSkillFile`'s own not-found signal). */
export async function loadSkill(skillDir: string, name: string): Promise<Skill | null> {
  const body = await readSkillFile(skillDir, name)
  if (body === null) return null
  const metas = await scanSkillDir(skillDir)
  const meta = metas.find(m => m.name === name)
  return {
    name: meta?.name ?? name,
    description: meta?.description,
    instructions: body,
  }
}
