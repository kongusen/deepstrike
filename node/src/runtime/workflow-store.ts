//! M6: file-backed persistence for declarative `WorkflowSpec`s — the SDK side of "save & share
//! workflows". A spec is pure data, so a saved workflow is plain JSON that round-trips exactly. Check
//! the files into `~/.deepstrike/workflows/`, or ship them inside a skill as templates: put the JSON
//! in the skill folder and have the agent `load()` + (optionally) tweak the spec before `runWorkflow`.

import { mkdir, writeFile, readFile, readdir } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"
import type { WorkflowSpec } from "../types/agent.js"

function defaultRoot(): string {
  return join(homedir(), ".deepstrike", "workflows")
}

/** Reject names that could escape the store directory; allow a safe slug only. */
function safeName(name: string): string {
  if (!/^[A-Za-z0-9_-]+$/.test(name)) {
    throw new Error(`invalid workflow name "${name}": use only letters, digits, "-", "_"`)
  }
  return name
}

/** File-backed `WorkflowSpec` store. Default root `~/.deepstrike/workflows`; override via `rootDir`
 *  (e.g. a skill folder for distribution). One spec per `<name>.json`. */
export class FileWorkflowStore {
  private readonly root: string

  constructor(opts?: { rootDir?: string }) {
    this.root = opts?.rootDir ?? defaultRoot()
  }

  /** Persist `spec` under `name`; returns the file path written. */
  async save(name: string, spec: WorkflowSpec): Promise<string> {
    const path = join(this.root, `${safeName(name)}.json`)
    await mkdir(this.root, { recursive: true })
    await writeFile(path, JSON.stringify(spec, null, 2), "utf8")
    return path
  }

  /** Load the spec saved under `name`. Throws if it does not exist. */
  async load(name: string): Promise<WorkflowSpec> {
    const path = join(this.root, `${safeName(name)}.json`)
    return JSON.parse(await readFile(path, "utf8")) as WorkflowSpec
  }

  /** The names of all saved workflows (sorted); `[]` when the store dir does not exist yet. */
  async list(): Promise<string[]> {
    try {
      const files = await readdir(this.root)
      return files.filter(f => f.endsWith(".json")).map(f => f.slice(0, -".json".length)).sort()
    } catch {
      return []
    }
  }
}
