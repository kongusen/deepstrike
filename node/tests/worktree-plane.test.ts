import { WorktreeExecutionPlane, type WorktreeManager } from "../src/runtime/worktree-plane.js"
import { LocalExecutionPlane, type ExecutionPlane, type RunContext } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import type { ToolCall, StreamEvent } from "../src/types.js"

/** Records create/remove calls; hands back a deterministic fake path (no real git). */
class FakeManager implements WorktreeManager {
  created: string[] = []
  removed: string[] = []
  async create(id: string): Promise<string> {
    this.created.push(id)
    return `/tmp/wt/${id}`
  }
  async remove(path: string): Promise<void> {
    this.removed.push(path)
  }
}

/** Inner plane that records the `cwd` each executeAll was given and emits one tool_result. */
class RecordingPlane implements ExecutionPlane {
  cwds: (string | undefined)[] = []
  register(): this {
    return this
  }
  unregister(): this {
    return this
  }
  schemas() {
    return []
  }
  async *executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent> {
    this.cwds.push(ctx.cwd)
    for (const c of calls) {
      yield { type: "tool_result", callId: c.id, name: c.name, content: "ok", isError: false } as StreamEvent
    }
  }
}

const drain = async (it: AsyncIterable<StreamEvent>) => {
  const out: StreamEvent[] = []
  for await (const e of it) out.push(e)
  return out
}
const call = (id: string): ToolCall => ({ id, name: "noop", arguments: "{}" })

describe("WorktreeExecutionPlane", () => {
  it("creates the worktree once, injects it as ctx.cwd, then removes it on cleanup", async () => {
    const mgr = new FakeManager()
    const inner = new RecordingPlane()
    const wt = new WorktreeExecutionPlane(inner, mgr, "wf-node3")

    expect(wt.worktreePath()).toBeUndefined()
    await drain(wt.executeAll([call("a")], {}))
    await drain(wt.executeAll([call("b")], {})) // second round: no second create

    expect(mgr.created).toEqual(["wf-node3"]) // created exactly once
    expect(wt.worktreePath()).toBe("/tmp/wt/wf-node3")
    expect(inner.cwds).toEqual(["/tmp/wt/wf-node3", "/tmp/wt/wf-node3"]) // cwd injected each call

    await wt.cleanup()
    expect(mgr.removed).toEqual(["/tmp/wt/wf-node3"])
    expect(wt.worktreePath()).toBeUndefined()
    await wt.cleanup() // idempotent — no second remove
    expect(mgr.removed).toEqual(["/tmp/wt/wf-node3"])
  })

  it("passes tool registration + schemas straight through to the inner plane", () => {
    const inner = new RecordingPlane()
    const wt = new WorktreeExecutionPlane(inner, new FakeManager(), "x")
    expect(wt.register()).toBe(wt)
    expect(wt.schemas()).toEqual([])
  })

  it("M3(a): the injected cwd reaches a tool's execute via ToolExecContext", async () => {
    // Full thread: WorktreeExecutionPlane injects ctx.cwd → LocalExecutionPlane.executeSingle passes
    // ctx → the tool's execute(args, ctx) reads it. This is what makes worktree isolation real.
    const inner = new LocalExecutionPlane()
    let seenCwd: string | undefined
    inner.register(
      tool("probe", "records cwd", { type: "object", properties: {} }, async (_args, ctx) => {
        seenCwd = ctx?.cwd
        return "ok"
      }),
    )
    const wt = new WorktreeExecutionPlane(inner, new FakeManager(), "wf-node7")
    await drain(wt.executeAll([{ id: "c1", name: "probe", arguments: "{}" }], {}))
    expect(seenCwd).toBe("/tmp/wt/wf-node7")
  })
})
