import { lstat, mkdir, readFile, readdir, writeFile } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"
import type { DynamicWorkflowScript } from "./dynamic.js"

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

function validateScript(value: unknown): DynamicWorkflowScript {
  if (!value || typeof value !== "object") throw new Error("invalid dynamic workflow artifact")
  const artifact = value as { meta?: unknown; source?: unknown }
  if (!artifact.meta || typeof artifact.meta !== "object") throw new Error("dynamic workflow artifact is missing meta")
  const meta = artifact.meta as { name?: unknown; description?: unknown; phases?: unknown; sizeGuideline?: unknown }
  if (typeof meta.name !== "string" || !meta.name.trim()) throw new Error("dynamic workflow meta.name must be a non-empty string")
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

  constructor(opts?: { rootDir?: string }) {
    this.root = opts?.rootDir ?? defaultRoot()
  }

  async save(name: string, script: DynamicWorkflowScript): Promise<string> {
    const safe = safeName(name)
    const validated = validateScript(script)
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
    return validateScript(JSON.parse(await readFile(path, "utf8")) as unknown)
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

