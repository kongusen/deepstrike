import { lstat, mkdir, readFile, readdir, writeFile } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"
import { createDynamicWorkflowArtifact, fingerprintDynamicWorkflowScript, type DynamicWorkflowArtifact, type DynamicWorkflowScript } from "./dynamic.js"

export interface DynamicWorkflowArtifactBundle {
  version: 1
  artifact: DynamicWorkflowArtifact
}

export interface DynamicWorkflowArtifactDescriptor {
  name: string
  digest: string
  origin: string
  scope: DynamicWorkflowArtifactScope
}

export type DynamicWorkflowArtifactScope = "project" | "user" | "plugin" | "package" | "custom"

function defaultRoot(): string {
  return join(homedir(), ".deepstrike", "workflows", "dynamic")
}

function safeName(name: string): string {
  if (!/^[A-Za-z0-9_-]+$/.test(name)) {
    throw new Error(`invalid dynamic workflow name "${name}": use only letters, digits, "-", "_"`)
  }
  return name
}

async function rejectSymlink(path: string, label: string): Promise<void> {
  try {
    if ((await lstat(path)).isSymbolicLink()) throw new Error(`refusing ${label} symlink: ${path}`)
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error
  }
}

function validateScript(value: unknown, expectedName?: string): DynamicWorkflowScript {
  if (!value || typeof value !== "object") throw new Error("invalid dynamic workflow artifact")
  const artifact = value as { meta?: unknown; source?: unknown }
  if (!artifact.meta || typeof artifact.meta !== "object") throw new Error("dynamic workflow artifact is missing meta")
  const meta = artifact.meta as { name?: unknown; description?: unknown; phases?: unknown; sizeGuideline?: unknown }
  if (typeof meta.name !== "string" || !meta.name.trim()) throw new Error("dynamic workflow meta.name must be a non-empty string")
  if (expectedName && meta.name !== expectedName) throw new Error(`dynamic workflow meta.name "${meta.name}" does not match artifact name "${expectedName}"`)
  if (typeof meta.description !== "string" || !meta.description.trim()) {
    throw new Error("dynamic workflow meta.description must be a non-empty string")
  }
  if (meta.phases !== undefined && (!Array.isArray(meta.phases) || meta.phases.some(phase => typeof phase !== "string"))) {
    throw new Error("dynamic workflow meta.phases must be an array of strings")
  }
  if (meta.sizeGuideline !== undefined && !["small", "medium", "large", "unrestricted"].includes(String(meta.sizeGuideline))) {
    throw new Error("dynamic workflow meta.sizeGuideline is invalid")
  }
  if (typeof artifact.source !== "string" || !artifact.source.trim()) throw new Error("dynamic workflow source must be non-empty")
  return {
    meta: {
      name: meta.name,
      description: meta.description,
      ...(meta.phases ? { phases: [...meta.phases] as string[] } : {}),
      ...(meta.sizeGuideline ? { sizeGuideline: meta.sizeGuideline as DynamicWorkflowScript["meta"]["sizeGuideline"] } : {}),
    },
    source: artifact.source,
  }
}

/** File-backed dynamic script artifacts. Source is data here; execution is owned by the later VM slice. */
export class FileDynamicWorkflowStore {
  private readonly root: string
  readonly scope: DynamicWorkflowArtifactScope
  readonly origin: string
  private readonly readOnly: boolean

  constructor(opts?: { rootDir?: string; scope?: DynamicWorkflowArtifactScope; origin?: string; readOnly?: boolean }) {
    this.root = opts?.rootDir ?? defaultRoot()
    this.scope = opts?.scope ?? "custom"
    this.origin = opts?.origin ?? "file-store"
    this.readOnly = opts?.readOnly ?? false
  }

  async save(name: string, script: DynamicWorkflowScript): Promise<string> {
    if (this.readOnly) throw new Error(`dynamic workflow store "${this.origin}" is read-only`)
    const safe = safeName(name)
    const validated = validateScript(script, safe)
    await this.ensureRoot()
    const path = join(this.root, `${safe}.json`)
    await rejectSymlink(path, "dynamic workflow artifact")
    await writeFile(path, JSON.stringify(validated, null, 2), "utf8")
    return path
  }

  async load(name: string): Promise<DynamicWorkflowScript> {
    const safe = safeName(name)
    await this.ensureRoot(false)
    const path = join(this.root, `${safe}.json`)
    await rejectSymlink(path, "dynamic workflow artifact")
    return validateScript(JSON.parse(await readFile(path, "utf8")) as unknown, safe)
  }

  async list(): Promise<string[]> {
    try {
      await rejectSymlink(this.root, "dynamic workflow store")
      const files = await readdir(this.root)
      return files.filter(file => file.endsWith(".json")).map(file => file.slice(0, -5)).sort()
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return []
      throw error
    }
  }

  private async ensureRoot(create = true): Promise<void> {
    await rejectSymlink(this.root, "dynamic workflow store")
    if (create) await mkdir(this.root, { recursive: true })
  }
}

/** A read-through catalog over ordered artifact stores; earlier stores win on duplicate names. */
export class DynamicWorkflowArtifactCatalog {
  constructor(private readonly stores: readonly FileDynamicWorkflowStore[]) {
    if (stores.length === 0) throw new Error("dynamic workflow artifact catalog requires at least one store")
  }

  async discover(): Promise<DynamicWorkflowArtifactDescriptor[]> {
    const seen = new Set<string>()
    const descriptors: DynamicWorkflowArtifactDescriptor[] = []
    for (const store of this.stores) {
      for (const name of await store.list()) {
        if (seen.has(name)) continue
        const artifact = await this.load(name)
        seen.add(name)
        descriptors.push({ name, digest: artifact.digest, origin: artifact.origin ?? store.origin, scope: store.scope })
      }
    }
    return descriptors.sort((left, right) => left.name.localeCompare(right.name))
  }

  async load(name: string, expectedDigest?: string): Promise<DynamicWorkflowArtifact> {
    for (const store of this.stores) {
      if (!(await store.list()).includes(name)) continue
      const script = await store.load(name)
      const artifact = createDynamicWorkflowArtifact(script, this.stores.find(candidate => candidate === store)?.origin ?? store.origin)
      if (expectedDigest && artifact.digest !== expectedDigest) {
        throw new Error(`dynamic workflow artifact "${name}" digest mismatch`)
      }
      return artifact
    }
    throw new Error(`dynamic workflow artifact "${name}" was not found`)
  }

  async distribute(name: string, destination: FileDynamicWorkflowStore): Promise<string> {
    const artifact = await this.load(name)
    return destination.save(artifact.name, artifact.script)
  }
}

/** Build the default discovery order: project workflows override user workflows, which override
 * plugin and package contributions. Every store remains independently addressable for distribution. */
export function createDynamicWorkflowArtifactCatalog(options: {
  projectRoot?: string
  userRoot?: string
  pluginRoots?: readonly string[]
  packageRoots?: readonly string[]
} = {}): DynamicWorkflowArtifactCatalog {
  const stores = [
    new FileDynamicWorkflowStore({ rootDir: options.projectRoot ?? join(process.cwd(), ".deepstrike", "workflows"), scope: "project", origin: "project" }),
    new FileDynamicWorkflowStore({ rootDir: options.userRoot ?? defaultRoot(), scope: "user", origin: "user" }),
    ...(options.pluginRoots ?? []).map(rootDir => new FileDynamicWorkflowStore({ rootDir, scope: "plugin", origin: `plugin:${rootDir}`, readOnly: true })),
    ...(options.packageRoots ?? []).map(rootDir => new FileDynamicWorkflowStore({ rootDir, scope: "package", origin: `package:${rootDir}`, readOnly: true })),
  ]
  return new DynamicWorkflowArtifactCatalog(stores)
}

export function encodeDynamicWorkflowArtifact(artifact: DynamicWorkflowArtifact): string {
  const script = validateScript(artifact.script)
  const expected = fingerprintDynamicWorkflowScript(script)
  if (artifact.name !== script.meta.name || artifact.digest !== expected) {
    throw new Error(`dynamic workflow artifact "${artifact.name}" has an invalid digest`)
  }
  return JSON.stringify({ version: 1, artifact: { ...artifact, script } }, null, 2)
}

export function decodeDynamicWorkflowArtifact(serialized: string): DynamicWorkflowArtifact {
  let bundle: unknown
  try { bundle = JSON.parse(serialized) as unknown } catch (error) { throw new Error("invalid dynamic workflow artifact bundle", { cause: error }) }
  if (!bundle || typeof bundle !== "object" || (bundle as { version?: unknown }).version !== 1) {
    throw new Error("unsupported dynamic workflow artifact bundle version")
  }
  const artifact = (bundle as { artifact?: unknown }).artifact
  if (!artifact || typeof artifact !== "object") throw new Error("dynamic workflow artifact bundle is missing artifact")
  const value = artifact as Partial<DynamicWorkflowArtifact>
  if (typeof value.name !== "string" || typeof value.digest !== "string" || !value.script || typeof value.script !== "object") {
    throw new Error("invalid dynamic workflow artifact bundle")
  }
  const script = validateScript(value.script)
  const expected = fingerprintDynamicWorkflowScript(script)
  if (value.name !== script.meta.name || value.digest !== expected) throw new Error("dynamic workflow artifact bundle digest mismatch")
  return createDynamicWorkflowArtifact(script, typeof value.origin === "string" ? value.origin : undefined)
}
