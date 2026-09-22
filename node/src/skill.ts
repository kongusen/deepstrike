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
  digest: string
  descriptor: SkillMetadata
}

export interface ResolvedSkillRevision extends SkillRevision {
  allowedTools?: string[]
}

/** @deprecated Kernel activation is not host authority. Use ResolvedSkillRevision. */
export type ActivatedSkill = ResolvedSkillRevision

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

/** One storage-independent L2 contract. Sources resolve identity before loading instructions. */
export interface SkillSource {
  list(context: SkillLoadContext): Promise<SkillMetadata[]>
  resolve(ref: SkillRef, context: SkillLoadContext): Promise<SkillRevision>
  load(revision: SkillRevision): Promise<SkillPackage>
  readResource(revision: SkillRevision, resource: SkillResourceRef): Promise<Uint8Array>
}

/** Resolve host material without claiming that the Kernel activated the Skill. */
export function resolveSkillRevision(pkg: SkillPackage, ref: SkillRef): ResolvedSkillRevision {
  return Object.freeze({
    ref: { name: ref.name, ...(pkg.descriptor.version ? { version: pkg.descriptor.version } : {}), ...(pkg.descriptor.digest ? { digest: pkg.descriptor.digest } : {}) },
    digest: pkg.descriptor.digest ?? "sha256:unknown",
    descriptor: { ...pkg.descriptor },
    ...(pkg.descriptor.allowedTools ? { allowedTools: [...pkg.descriptor.allowedTools] } : {}),
  })
}

/** @deprecated Host code must not mint activation facts. Use resolveSkillRevision. */
export const activateSkill = resolveSkillRevision

function descriptorFromSkill(skill: Skill): SkillMetadata {
  return projectSkillMetadata(skill)
}

function packageFromInline(skill: Skill): SkillPackage {
  const resources: SkillPackage["resources"] = { scripts: [], references: [], assets: [] }
  for (const resource of skill.resources ?? []) {
    const kind = resource.metadata?.kind
    if (kind === "scripts" || kind === "references" || kind === "assets") {
      resources[kind].push({ path: resource.uri ?? resource.name, kind })
    }
  }
  const digest = skill.metadata?.digest ? String(skill.metadata.digest) : `sha256:inline:${skill.name}`
  return { descriptor: { ...descriptorFromSkill(skill), digest }, instructions: skill.instructions ?? "", resources }
}

export class InlineSkillSource implements SkillSource {
  private readonly revisions = new Map<string, { skill: Skill; pkg: SkillPackage }>()
  constructor(private readonly skills: readonly Skill[]) {
    for (const skill of skills) {
      const pkg = packageFromInline(skill)
      this.revisions.set(skill.name, { skill: structuredClone(skill), pkg })
    }
  }
  async list(_context: SkillLoadContext): Promise<SkillMetadata[]> {
    return [...this.revisions.values()].map(({ pkg }) => ({ ...pkg.descriptor }))
  }
  async resolve(ref: SkillRef, _context: SkillLoadContext): Promise<SkillRevision> {
    const item = this.revisions.get(ref.name)
    if (!item) throw new Error(`skill "${ref.name}" is not available in the inline source`)
    if (ref.digest && ref.digest !== item.pkg.descriptor.digest) throw new Error(`skill "${ref.name}" does not match requested digest`)
    return { ref: { ...ref }, digest: item.pkg.descriptor.digest!, descriptor: { ...item.pkg.descriptor } }
  }
  async load(revision: SkillRevision): Promise<SkillPackage> {
    const item = this.revisions.get(revision.ref.name)
    if (!item) throw new Error(`skill "${revision.ref.name}" is not available in the inline source`)
    return structuredClone(item.pkg)
  }
  async readResource(_revision: SkillRevision, _resource: SkillResourceRef): Promise<Uint8Array> {
    throw new Error("inline skill resources must be supplied as content-bearing declarations")
  }
}

export class DirectorySkillSource implements SkillSource {
  constructor(
    private readonly root: string,
    private readonly options: { scope?: (context: SkillLoadContext) => string | string[] } = {},
  ) {}

  private scopedRoot(context: SkillLoadContext): string {
    if (!this.options.scope) return this.root
    const scoped = this.options.scope(context)
    const parts = Array.isArray(scoped) ? scoped : scoped.split(/[\\/]+/)
    for (const value of parts) if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("skill source scope contains an unsafe segment")
    return path.join(this.root, ...parts)
  }

  private legacyScopedRoot(context: SkillLoadContext): string {
    for (const value of [context.userId, context.tenantId, context.namespace]) {
      if (value !== undefined && !/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("skill source scope contains an unsafe segment")
    }
    return path.join(this.root, context.tenantId ?? "default", context.userId, ...(context.namespace ? [context.namespace] : []))
  }

  private readonly revisions = new WeakMap<object, SkillPackage>()

  private async loadFromRoots(ref: SkillRef, context: SkillLoadContext): Promise<SkillPackage | null> {
    const primary = await loadSkillPackage(this.scopedRoot(context), ref.name)
    if (primary || this.options.scope) return primary
    return loadSkillPackage(this.legacyScopedRoot(context), ref.name)
  }

  async list(context: SkillLoadContext): Promise<SkillMetadata[]> {
    const primary = await scanSkillDir(this.scopedRoot(context))
    if (primary.length || this.options.scope) return primary
    return scanSkillDir(this.legacyScopedRoot(context))
  }

  async resolve(ref: SkillRef, context: SkillLoadContext): Promise<SkillRevision> {
    const pkg = await this.loadFromRoots(ref, context)
    if (!pkg) throw new Error(`skill "${ref.name}" was not found for user "${context.userId}"`)
    if (ref.version && pkg.descriptor.version !== ref.version) throw new Error(`skill "${ref.name}" does not match requested version "${ref.version}"`)
    if (ref.digest && pkg.descriptor.digest !== ref.digest) throw new Error(`skill "${ref.name}" does not match requested digest`)
    const revision = { ref: { ...ref }, digest: pkg.descriptor.digest!, descriptor: { ...pkg.descriptor } }
    this.revisions.set(revision, pkg)
    return revision
  }

  async load(revision: SkillRevision | SkillRef, context?: SkillLoadContext): Promise<SkillPackage> {
    if (!("descriptor" in revision)) {
      if (!context) throw new Error(`skill revision "${revision.name}" requires a load context`)
      const resolved = await this.resolve(revision, context)
      return this.load(resolved)
    }
    const pkg = this.revisions.get(revision)
    if (!pkg) throw new Error(`skill revision "${revision.ref.name}" is not owned by this source`)
    return pkg
  }

  readResource(revision: SkillRevision | SkillPackage, resource: SkillResourceRef): Promise<Uint8Array> {
    const pkg = "descriptor" in revision && "instructions" in revision
      ? revision
      : this.revisions.get(revision)
    if (!pkg) return Promise.reject(new Error(`skill revision is not owned by this source`))
    return readSkillResource(pkg, resource)
  }
}

/** Generic source adapter for database, Git, and remote stores. */
export class ResolverSkillSource implements SkillSource {
  private readonly revisions = new Map<string, SkillPackage>()
  constructor(
    private readonly resolver: (ref: SkillRef, context: SkillLoadContext) => Promise<SkillPackage>,
    private readonly lister: (context: SkillLoadContext) => Promise<SkillMetadata[]>,
  ) {}
  list(context: SkillLoadContext): Promise<SkillMetadata[]> { return this.lister(context) }
  async resolve(ref: SkillRef, context: SkillLoadContext): Promise<SkillRevision> {
    const pkg = await this.resolver(ref, context)
    if (ref.digest && ref.digest !== pkg.descriptor.digest) throw new Error(`skill "${ref.name}" does not match requested digest`)
    const revision = { ref: { ...ref }, digest: pkg.descriptor.digest ?? `sha256:unidentified:${ref.name}`, descriptor: { ...pkg.descriptor } }
    this.revisions.set(revision.digest, pkg)
    return revision
  }
  async load(revision: SkillRevision): Promise<SkillPackage> {
    const pkg = this.revisions.get(revision.digest)
    if (!pkg) throw new Error(`skill revision "${revision.ref.name}" is not owned by this source`)
    return pkg
  }
  async readResource(revision: SkillRevision, resource: SkillResourceRef): Promise<Uint8Array> {
    const pkg = await this.load(revision)
    return readSkillResource(pkg, resource)
  }
}

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
