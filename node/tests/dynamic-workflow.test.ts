import {
  DynamicWorkflowExecutor,
  DynamicWorkflowCancellationError,
  DynamicWorkflowLimitError,
  dynamicAgentTask,
  resolveDynamicWorkflowLimits,
} from "../src/workflow/dynamic.js"
import type { DynamicWorkflowHost } from "../src/workflow/dynamic.js"
import type { WorkflowNodeSpec } from "../src/types/agent.js"

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

  it("admits declarative parallel agent tasks as kernel batches", async () => {
    const batchSizes: number[] = []
    const host: DynamicWorkflowHost = {
      async runWorkflow(spec) {
        batchSizes.push(spec.nodes.length)
        return {
          nodeOutcomes: spec.nodes.map(node => ({
            nodeId: node.nodeId!,
            status: "completed" as const,
            output: { role: "assistant" as const, content: `batch:${node.task as string}` },
          })),
          outputs: Object.fromEntries(spec.nodes.map(node => [node.nodeId!, `batch:${node.task as string}`])),
        }
      },
    }
    const executor = new DynamicWorkflowExecutor(host, { limits: { maxConcurrentAgents: 3 } })
    const run = await executor.run(ctx => ctx.parallelAgents(["a", "b", "c"], item => dynamicAgentTask(`task:${item}`)))

    expect(batchSizes).toEqual([3])
    expect(run.value.map(result => result?.text)).toEqual(["batch:task:a", "batch:task:b", "batch:task:c"])
  })

  it("keeps host target, model, trust, and tool policy on the dynamic spawn boundary", async () => {
    let captured: WorkflowNodeSpec | undefined
    const host: DynamicWorkflowHost = {
      async runWorkflow(spec) {
        captured = spec.nodes[0]
        return {
          nodeOutcomes: [{ nodeId: captured.nodeId!, status: "completed", output: { role: "assistant", content: "ok" } }],
          outputs: { [captured.nodeId!]: "ok" },
        }
      },
    }
    const executor = new DynamicWorkflowExecutor(host, { runId: "dynamic-boundary" })
    await executor.run(ctx => ctx.agent("inspect", {
      agent: "researcher",
      modelHint: "fast",
      trust: "quarantined",
      toolAccess: "filtered",
    }))

    expect(captured).toMatchObject({
      agent: "researcher",
      modelHint: "fast",
      trust: "quarantined",
      toolAccess: "filtered",
    })
  })

  it("rejects unsafe limits and oversized batches before spawning", async () => {
    expect(resolveDynamicWorkflowLimits()).toEqual({ maxConcurrentAgents: 16, maxAgentsPerRun: 1000, maxItemsPerBatch: 4096 })
    expect(() => resolveDynamicWorkflowLimits({ maxConcurrentAgents: 257 })).toThrow(DynamicWorkflowLimitError)

    const executor = new DynamicWorkflowExecutor(fakeHost(), { limits: { maxItemsPerBatch: 2 } })
    await expect(executor.run(ctx => ctx.parallel([1, 2, 3], item => item))).rejects.toThrow(/at most 2 items/)
  })

  it("cancels an in-flight script through AbortSignal and records one terminal event", async () => {
    const controller = new AbortController()
    const events: string[] = []
    const executor = new DynamicWorkflowExecutor(fakeHost(), {
      signal: controller.signal,
      onLifecycleEvent: event => events.push(event.kind),
    })
    const run = executor.run(async () => new Promise(resolve => setTimeout(() => resolve("late"), 100)))
    setTimeout(() => controller.abort(new Error("user stopped")), 5)
    await expect(run).rejects.toBeInstanceOf(DynamicWorkflowCancellationError)
    expect(events.at(-1)).toBe("run_cancelled")
  })
})
