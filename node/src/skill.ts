import { loadSkillPackage, readSkillFile, readSkillResource, scanSkillDir, type SkillMetadata, type SkillPackage, type SkillResourceRef } from "./skills/loader.js"
import path from "node:path"
export type { SkillPackage } from "./skills/loader.js"

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
  /** Capability requirements are distinct from bundled package resources. */
  requires?: {
    tools?: SkillTool[]
    mcpServers?: SkillMCPServer[]
    knowledge?: SkillKnowledge[]
  }
}

export interface SkillRevision {
  ref: SkillRef
  digest?: string
}

export interface ActivatedSkill extends SkillRevision {
  activationId: string
  activatedAt: number
  allowedTools?: string[]
}

/** A source-independent public reference. The runtime resolves it inside a user/tenant scope. */
export interface SkillRef {
  name: string
  version?: string
  digest?: string
}

export type SkillRefInput = string | SkillRef
export type SkillDeclaration = Skill | SkillRefInput

function isSkillObject(value: SkillDeclaration): value is Skill {
  return typeof value === "object" && ["description", "instructions", "resources", "scripts", "tools", "mcpServers", "knowledge", "metadata", "providerOptions", "requires"].some(key => key in value)
}

export function normalizeSkillRef(input: SkillRefInput): SkillRef {
  if (typeof input === "string") return { name: input }
  return { name: input.name, ...(input.version ? { version: input.version } : {}), ...(input.digest ? { digest: input.digest } : {}) }
}

/** Named L1 crossing: inline declarations become source-independent runtime requirements. */
export function projectSkillRequirement(input: SkillDeclaration): SkillRef {
  if (!isSkillObject(input)) return normalizeSkillRef(input)
  return { name: input.name }
}

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

/** Storage-neutral L2 source. It exposes packages and lazy resource reads, never source details. */
export interface SkillSource {
  list(context: SkillLoadContext): Promise<SkillMetadata[]>
  load(ref: SkillRef, context: SkillLoadContext): Promise<SkillPackage>
  readResource(pkg: SkillPackage, resource: SkillResourceRef): Promise<Uint8Array>
}

/** Freeze a resolved package into the run-scoped activation fact. */
export function activateSkill(pkg: SkillPackage, ref: SkillRef): ActivatedSkill {
  return Object.freeze({
    ref: { name: ref.name, ...(pkg.descriptor.version ? { version: pkg.descriptor.version } : {}), ...(pkg.descriptor.digest ? { digest: pkg.descriptor.digest } : {}) },
    digest: pkg.descriptor.digest,
    activationId: `skill-${crypto.randomUUID()}`,
    activatedAt: Date.now(),
    ...(pkg.descriptor.allowedTools ? { allowedTools: [...pkg.descriptor.allowedTools] } : {}),
  })
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
    const pkg = await loadSkillPackage(this.scopedRoot(context), ref.name)
    if (pkg) {
      if (ref.version && pkg.descriptor.version !== ref.version) throw new Error(`skill "${ref.name}" does not match requested version "${ref.version}"`)
      if (ref.digest && pkg.descriptor.digest !== ref.digest) throw new Error(`skill "${ref.name}" does not match requested digest`)
      return {
        name: pkg.descriptor.name,
        description: pkg.descriptor.description,
        instructions: pkg.instructions,
        resources: Object.values(pkg.resources).flat().map(resource => ({ name: resource.path, uri: resource.path, metadata: { kind: resource.kind } })),
        metadata: { version: pkg.descriptor.version, digest: pkg.descriptor.digest },
      }
    }
    const skill = await loadSkill(this.scopedRoot(context), ref.name)
    if (!skill) throw new Error(`skill "${ref.name}" was not found for user "${context.userId}"`)
    return skill
  }
}

export class DirectorySkillSource implements SkillSource {
  constructor(private readonly root: string) {}

  private scopedRoot(context: SkillLoadContext): string {
    for (const value of [context.userId, context.tenantId, context.namespace]) {
      if (value !== undefined && !/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("skill source scope contains an unsafe segment")
    }
    return path.join(this.root, context.tenantId ?? "default", context.userId, ...(context.namespace ? [context.namespace] : []))
  }

  list(context: SkillLoadContext): Promise<SkillMetadata[]> { return scanSkillDir(this.scopedRoot(context)) }

  async load(ref: SkillRef, context: SkillLoadContext): Promise<SkillPackage> {
    const pkg = await loadSkillPackage(this.scopedRoot(context), ref.name)
    if (!pkg) throw new Error(`skill "${ref.name}" was not found for user "${context.userId}"`)
    if (ref.version && pkg.descriptor.version !== ref.version) throw new Error(`skill "${ref.name}" does not match requested version "${ref.version}"`)
    if (ref.digest && pkg.descriptor.digest !== ref.digest) throw new Error(`skill "${ref.name}" does not match requested digest`)
    return pkg
  }

  readResource(pkg: SkillPackage, resource: SkillResourceRef): Promise<Uint8Array> { return readSkillResource(pkg, resource) }
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
