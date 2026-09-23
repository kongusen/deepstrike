import { createHash, randomUUID } from "node:crypto"
import { lstat, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"
import type { DynamicWorkflowAgentOptions } from "./dynamic.js"
import type { DynamicWorkflowLifecycleEvent, DynamicWorkflowLimits } from "./dynamic.js"
import type { WorkflowNodeStatus } from "../types/agent.js"

export interface DynamicWorkflowInvocationRecord {
  nodeId: string
  promptFingerprint: string
  text: string
  status: WorkflowNodeStatus | "cancelled"
  termination?: string
  error?: string
}

export interface DynamicWorkflowReplayRun {
  version: 2
  runId: string
  inputFingerprint?: string
  artifactDigest?: string
  argsFingerprint?: string
  limits?: Required<DynamicWorkflowLimits>
  status: "planning" | "running" | "completed" | "failed" | "cancelled"
  events: DynamicWorkflowLifecycleEvent[]
  records: DynamicWorkflowInvocationRecord[]
}

export interface DynamicWorkflowReplayStore {
  find(runId: string, nodeId: string, promptFingerprint: string): Promise<DynamicWorkflowInvocationRecord | undefined>
  save(runId: string, record: DynamicWorkflowInvocationRecord): Promise<void>
  loadRun?(runId: string): Promise<DynamicWorkflowReplayRun | undefined>
  saveRun?(runId: string, run: DynamicWorkflowReplayRun): Promise<void>
  appendEvent?(runId: string, event: DynamicWorkflowLifecycleEvent): Promise<void>
}

export function fingerprintDynamicWorkflowRun(input: {
  artifactDigest?: string
  args: Record<string, unknown>
  limits: Required<DynamicWorkflowLimits>
}): string {
  return createHash("sha256").update(JSON.stringify({
    artifactDigest: input.artifactDigest ?? "inline",
    args: input.args,
    limits: input.limits,
  })).digest("hex")
}

export function fingerprintDynamicWorkflowInvocation(prompt: string, options: DynamicWorkflowAgentOptions): string {
  const normalized = JSON.stringify({
    prompt,
    options: Object.fromEntries(Object.entries(options).sort(([left], [right]) => left.localeCompare(right))),
  })
  return createHash("sha256").update(normalized).digest("hex")
}

export class InMemoryDynamicWorkflowReplayStore implements DynamicWorkflowReplayStore {
  private readonly runs = new Map<string, DynamicWorkflowReplayRun>()

  async find(runId: string, nodeId: string, promptFingerprint: string): Promise<DynamicWorkflowInvocationRecord | undefined> {
    return this.runs.get(runId)?.records.find(record => (record.status === "completed" || record.status === "completed_partial") && record.nodeId === nodeId && record.promptFingerprint === promptFingerprint)
  }

  async save(runId: string, record: DynamicWorkflowInvocationRecord): Promise<void> {
    const run = this.runs.get(runId) ?? { version: 2, runId, status: "running", events: [], records: [] }
    const index = run.records.findIndex(existing => existing.nodeId === record.nodeId)
    if (index >= 0) run.records[index] = { ...record }
    else run.records.push({ ...record })
    this.runs.set(runId, run)
  }

  async loadRun(runId: string): Promise<DynamicWorkflowReplayRun | undefined> {
    const run = this.runs.get(runId)
    return run ? structuredClone(run) : undefined
  }

  async saveRun(runId: string, run: DynamicWorkflowReplayRun): Promise<void> {
    this.runs.set(runId, structuredClone(run))
  }

  async appendEvent(runId: string, event: DynamicWorkflowLifecycleEvent): Promise<void> {
    const run = this.runs.get(runId) ?? { version: 2, runId, status: "running", events: [], records: [] }
    run.events.push(structuredClone(event))
    this.runs.set(runId, run)
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
  private readonly runTails = new Map<string, Promise<void>>()

  constructor(opts?: { rootDir?: string }) {
    this.root = opts?.rootDir ?? defaultRoot()
  }

  async find(runId: string, nodeId: string, promptFingerprint: string): Promise<DynamicWorkflowInvocationRecord | undefined> {
    const safe = safeRunId(runId)
    return this.withRunLock(safe, async () => {
      const run = await this.readRun(safe)
      return run.records.find(record => (record.status === "completed" || record.status === "completed_partial") && record.nodeId === nodeId && record.promptFingerprint === promptFingerprint)
    })
  }

  async save(runId: string, record: DynamicWorkflowInvocationRecord): Promise<void> {
    const safe = safeRunId(runId)
    await this.withRunLock(safe, async () => {
      await this.ensureRoot()
      await rejectSymlink(this.pathFor(safe), "dynamic workflow replay file")
      const run = await this.readRun(safe)
      const index = run.records.findIndex(existing => existing.nodeId === record.nodeId)
      if (index >= 0) run.records[index] = { ...record }
      else run.records.push({ ...record })
      await this.writeRun(safe, run)
    })
  }

  async loadRun(runId: string): Promise<DynamicWorkflowReplayRun | undefined> {
    const safe = safeRunId(runId)
    return this.withRunLock(safe, async () => {
      const run = await this.readRun(safe)
      return run.records.length || run.events.length || run.inputFingerprint ? run : undefined
    })
  }

  async saveRun(runId: string, run: DynamicWorkflowReplayRun): Promise<void> {
    const safe = safeRunId(runId)
    await this.withRunLock(safe, async () => {
      await this.ensureRoot()
      await rejectSymlink(this.pathFor(safe), "dynamic workflow replay file")
      await this.writeRun(safe, structuredClone(run))
    })
  }

  async appendEvent(runId: string, event: DynamicWorkflowLifecycleEvent): Promise<void> {
    const safe = safeRunId(runId)
    await this.withRunLock(safe, async () => {
      await this.ensureRoot()
      const run = await this.readRun(safe)
      run.events.push(event)
      await this.writeRun(safe, run)
    })
  }

  private pathFor(runId: string): string {
    return join(this.root, `${runId}.json`)
  }

  private async readRun(runId: string): Promise<DynamicWorkflowReplayRun> {
    const safe = safeRunId(runId)
    await rejectSymlink(this.root, "dynamic workflow replay store")
    try {
      const raw = JSON.parse(await readFile(this.pathFor(safe), "utf8")) as { version?: unknown; records?: unknown; events?: unknown }
      if (raw.version !== 2 || !Array.isArray(raw.records) || !Array.isArray(raw.events)) throw new Error(`invalid dynamic workflow replay file: ${this.pathFor(safe)}`)
      return raw as DynamicWorkflowReplayRun
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return { version: 2, runId, status: "running", events: [], records: [] }
      throw error
    }
  }

  private async writeRun(runId: string, run: DynamicWorkflowReplayRun): Promise<void> {
    const path = this.pathFor(runId)
    const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`
    try {
      await writeFile(temporary, JSON.stringify(run, null, 2), "utf8")
      await rename(temporary, path)
    } finally {
      await rm(temporary, { force: true })
    }
  }

  private async withRunLock<T>(runId: string, operation: () => Promise<T>): Promise<T> {
    const previous = this.runTails.get(runId) ?? Promise.resolve()
    const current = previous.catch(() => undefined).then(operation)
    const settled = current.then(() => undefined, () => undefined)
    this.runTails.set(runId, settled)
    try {
      return await current
    } finally {
      if (this.runTails.get(runId) === settled) this.runTails.delete(runId)
    }
  }

  private async ensureRoot(): Promise<void> {
    await rejectSymlink(this.root, "dynamic workflow replay store")
    await mkdir(this.root, { recursive: true })
  }
}
