//! M3/G4: per-sub-agent git-worktree isolation as an execution-plane decorator.
//!
//! An `isolation: "worktree"` workflow node should run its tools in its own working tree so parallel
//! write-capable nodes (the migration / refactor / evals patterns) don't clobber each other. This
//! module owns the *worktree lifecycle*: create one git worktree per sub-agent, inject its path as
//! `RunContext.cwd` so a cwd-aware inner plane scopes its work there, and remove it when the
//! sub-agent finishes. The git operations are behind an injectable [`WorktreeManager`] so the plane
//! is testable without mutating a real repository.

import { execFile } from "node:child_process"
import { promisify } from "node:util"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import type { RegisteredTool } from "../tools/index.js"
import type { ToolCall, ToolSchema, StreamEvent } from "../types.js"

const execFileAsync = promisify(execFile)

/** Creates and removes the worktree directory for one sub-agent. Injectable so the plane can be
 *  unit-tested without a real git repo (the default is git-backed). */
export interface WorktreeManager {
  /** Create the working directory for sub-agent `id`; returns its absolute path. */
  create(id: string): Promise<string>
  /** Remove the working directory previously created at `path`. Must not throw on a missing path. */
  remove(path: string): Promise<void>
}

/** Default git-backed manager: `git worktree add --detach <root>/<id> <ref>` then `git worktree
 *  remove --force`. Falls back to a plain recursive delete if `worktree remove` fails (e.g. the dir
 *  was already detached), so cleanup is best-effort and never throws. */
export class GitWorktreeManager implements WorktreeManager {
  constructor(private readonly opts: { repoRoot?: string; ref?: string; rootDir?: string } = {}) {}

  async create(id: string): Promise<string> {
    const root = this.opts.rootDir ?? (await mkdtemp(join(tmpdir(), "deepstrike-wt-")))
    const path = join(root, id)
    const ref = this.opts.ref ?? "HEAD"
    await execFileAsync("git", ["worktree", "add", "--detach", path, ref], { cwd: this.opts.repoRoot })
    return path
  }

  async remove(path: string): Promise<void> {
    try {
      await execFileAsync("git", ["worktree", "remove", "--force", path], { cwd: this.opts.repoRoot })
    } catch {
      await rm(path, { recursive: true, force: true }).catch(() => {})
    }
  }
}

/** Decorator plane: lazily creates a worktree on first execution, injects it as `RunContext.cwd` for
 *  every delegated call, and removes it on [`cleanup`]. Tool registration/schemas pass straight
 *  through to the inner plane. The worktree only *isolates* to the extent the inner plane honors
 *  `ctx.cwd` (e.g. a subprocess plane rooting commands there). */
export class WorktreeExecutionPlane implements ExecutionPlane {
  private path: string | undefined

  constructor(
    private readonly inner: ExecutionPlane,
    private readonly manager: WorktreeManager,
    private readonly id: string,
  ) {}

  register(...tools: RegisteredTool[]): this {
    this.inner.register(...tools)
    return this
  }

  unregister(name: string): this {
    this.inner.unregister(name)
    return this
  }

  schemas(): ToolSchema[] {
    return this.inner.schemas()
  }

  /** The created worktree path, or undefined before the first `executeAll` / after `cleanup`. */
  worktreePath(): string | undefined {
    return this.path
  }

  async *executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent> {
    if (this.path === undefined) this.path = await this.manager.create(this.id)
    yield* this.inner.executeAll(calls, { ...ctx, cwd: this.path })
  }

  /** Remove the worktree (idempotent — safe to call when none was created). */
  async cleanup(): Promise<void> {
    if (this.path === undefined) return
    const p = this.path
    this.path = undefined
    await this.manager.remove(p)
  }
}
