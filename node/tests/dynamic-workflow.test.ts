import {
  DynamicWorkflowExecutor,
  DynamicWorkflowLimitError,
  resolveDynamicWorkflowLimits,
} from "../src/workflow/dynamic.js"
import type { DynamicWorkflowHost } from "../src/workflow/dynamic.js"

function fakeHost(delayMs = 0, onRun?: (goal: string) => void, onDone?: (goal: string) => void): DynamicWorkflowHost {
  return {
    async runWorkflow(spec) {
      const node = spec.nodes[0]
      const goal = typeof node.task === "string" ? node.task : node.task.goal
      onRun?.(goal)
      if (delayMs) await new Promise(resolve => setTimeout(resolve, delayMs))
      onDone?.(goal)
      const content = goal.startsWith("json:") ? JSON.stringify({ goal }) : `done:${goal}`
      return {
        nodeOutcomes: [{
          nodeId: node.nodeId ?? "unknown",
          status: "completed",
          output: { role: "assistant", content },
        }],
        outputs: { [node.nodeId ?? "unknown"]: content },
      }
    },
  }
}

describe("DynamicWorkflowExecutor", () => {
  it("keeps args immutable and exposes phases, logs, pipeline, and structured agent values", async () => {
    const args = { paths: ["a.ts", "b.ts"] }
    let capturedArgs: Readonly<typeof args> | undefined
    const progress: string[] = []
    const executor = new DynamicWorkflowExecutor(fakeHost(), {
      args,
      runId: "dw-test",
      onProgress: state => progress.push(`${state.status}:${state.phase ?? ""}`),
    })

    const run = await executor.run(async ctx => {
      capturedArgs = ctx.args
      ctx.log("starting", { count: ctx.args.paths.length })
      await ctx.phase("audit", async () => {
        const first = await ctx.agent<{ goal: string }>("json:one", {
          outputSchema: { type: "object" },
        })
        expect(first?.value).toEqual({ goal: "json:one" })
      })
      return ctx.pipeline(["a", "b"], async item => (await ctx.agent(`check:${item}`))?.text)
    })

    expect(run.runId).toBe("dw-test")
    expect(run.value).toEqual(["done:check:a", "done:check:b"])
    expect(run.progress.status).toBe("completed")
    expect(run.progress.phases).toEqual([{ name: "audit", status: "completed", agentsStarted: 1, agentsCompleted: 1 }])
    expect(run.progress.logs[0].message).toBe("starting")
    expect(progress).toContain("running:audit")
    args.paths.push("mutated-after-start.ts")
    expect(capturedArgs?.paths).toEqual(["a.ts", "b.ts"])
    expect(run.progress.agentsStarted).toBe(3)
  })

  it("bounds parallel workers and preserves input order", async () => {
    let active = 0
    let peak = 0
    const host = fakeHost(5, () => {
      active += 1
      peak = Math.max(peak, active)
    }, () => { active -= 1 })
    const executor = new DynamicWorkflowExecutor(host, { limits: { maxConcurrentAgents: 2 } })
    const run = await executor.run(ctx => ctx.parallel([1, 2, 3, 4], async item => (await ctx.agent(`item:${item}`))?.text))

    expect(run.value).toEqual(["done:item:1", "done:item:2", "done:item:3", "done:item:4"])
    expect(run.progress.agentsStarted).toBe(4)
    expect(peak).toBeLessThanOrEqual(2)
  })

  it("bounds nested agent calls by real agent concurrency, not worker count", async () => {
    let active = 0
    let peak = 0
    const host = fakeHost(5, () => {
      active += 1
      peak = Math.max(peak, active)
    }, () => { active -= 1 })
    const executor = new DynamicWorkflowExecutor(host, { limits: { maxConcurrentAgents: 2 } })
    await executor.run(ctx => ctx.parallel([1, 2], async item =>
      Promise.all([ctx.agent(`nested:${item}:a`), ctx.agent(`nested:${item}:b`)]),
    ))

    expect(peak).toBeLessThanOrEqual(2)
  })

  it("rejects unsafe limits and oversized batches before spawning", async () => {
    expect(resolveDynamicWorkflowLimits()).toEqual({ maxConcurrentAgents: 16, maxAgentsPerRun: 1000, maxItemsPerBatch: 4096 })
    expect(() => resolveDynamicWorkflowLimits({ maxConcurrentAgents: 257 })).toThrow(DynamicWorkflowLimitError)

    const executor = new DynamicWorkflowExecutor(fakeHost(), { limits: { maxItemsPerBatch: 2 } })
    await expect(executor.run(ctx => ctx.parallel([1, 2, 3], item => item))).rejects.toThrow(/at most 2 items/)
  })
})
