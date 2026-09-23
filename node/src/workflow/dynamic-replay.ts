import { createHash } from "node:crypto"
import { lstat, mkdir, readFile, writeFile } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"
import type { DynamicWorkflowAgentOptions } from "./dynamic.js"

export interface DynamicWorkflowInvocationRecord {
  nodeId: string
  promptFingerprint: string
  text: string
  status: "completed" | "completed_partial"
  termination?: string
}

export interface DynamicWorkflowReplayStore {
  find(runId: string, nodeId: string, promptFingerprint: string): Promise<DynamicWorkflowInvocationRecord | undefined>
  save(runId: string, record: DynamicWorkflowInvocationRecord): Promise<void>
}

export function fingerprintDynamicWorkflowInvocation(prompt: string, options: DynamicWorkflowAgentOptions): string {
  const normalized = JSON.stringify({
    prompt,
    options: Object.fromEntries(Object.entries(options).sort(([left], [right]) => left.localeCompare(right))),
  })
  return createHash("sha256").update(normalized).digest("hex")
}

export class InMemoryDynamicWorkflowReplayStore implements DynamicWorkflowReplayStore {
  private readonly runs = new Map<string, DynamicWorkflowInvocationRecord[]>()

  async find(runId: string, nodeId: string, promptFingerprint: string): Promise<DynamicWorkflowInvocationRecord | undefined> {
    return this.runs.get(runId)?.find(record => record.nodeId === nodeId && record.promptFingerprint === promptFingerprint)
  }

  async save(runId: string, record: DynamicWorkflowInvocationRecord): Promise<void> {
    const records = this.runs.get(runId) ?? []
    const index = records.findIndex(existing => existing.nodeId === record.nodeId)
    if (index >= 0) records[index] = { ...record }
    else records.push({ ...record })
    this.runs.set(runId, records)
  }
}

function defaultRoot(): string {
  return join(homedir(), ".deepstrike", "workflows", "runs")
}

async function rejectSymlink(path: string, label: string): Promise<void> {
  try {
    if ((await lstat(path)).isSymbolicLink()) throw new Error(`refusing ${label} symlink: ${path}`)
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error
  }
}

function safeRunId(runId: string): string {
  if (!/^[A-Za-z0-9_-]+$/.test(runId)) throw new Error(`invalid dynamic workflow run id "${runId}"`)
  return runId
}

/** File-backed completed invocation results. Failed or running calls are never reused. */
export class FileDynamicWorkflowReplayStore implements DynamicWorkflowReplayStore {
  private readonly root: string

  constructor(opts?: { rootDir?: string }) {
    this.root = opts?.rootDir ?? defaultRoot()
  }

  async find(runId: string, nodeId: string, promptFingerprint: string): Promise<DynamicWorkflowInvocationRecord | undefined> {
    const records = await this.readRun(runId)
    return records.find(record => record.nodeId === nodeId && record.promptFingerprint === promptFingerprint)
  }

  async save(runId: string, record: DynamicWorkflowInvocationRecord): Promise<void> {
    const safe = safeRunId(runId)
    await this.ensureRoot()
    await rejectSymlink(this.pathFor(safe), "dynamic workflow replay file")
    const records = await this.readRun(safe)
    const index = records.findIndex(existing => existing.nodeId === record.nodeId)
    if (index >= 0) records[index] = { ...record }
    else records.push({ ...record })
    await writeFile(this.pathFor(safe), JSON.stringify({ version: 1, records }, null, 2), "utf8")
  }

  private pathFor(runId: string): string {
    return join(this.root, `${runId}.json`)
  }

  private async readRun(runId: string): Promise<DynamicWorkflowInvocationRecord[]> {
    const safe = safeRunId(runId)
    await rejectSymlink(this.root, "dynamic workflow replay store")
    try {
      const raw = JSON.parse(await readFile(this.pathFor(safe), "utf8")) as { version?: unknown; records?: unknown }
      if (raw.version !== 1 || !Array.isArray(raw.records)) throw new Error(`invalid dynamic workflow replay file: ${this.pathFor(safe)}`)
      return raw.records as DynamicWorkflowInvocationRecord[]
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return []
      throw error
    }
  }

  private async ensureRoot(): Promise<void> {
    await rejectSymlink(this.root, "dynamic workflow replay store")
    await mkdir(this.root, { recursive: true })
  }
}

