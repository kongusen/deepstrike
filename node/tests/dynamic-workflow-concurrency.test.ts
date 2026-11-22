/**
 * Dynamic workflow concurrency and replay identity.
 *
 * - `agent()` calls made in the same turn reach the host as one batch, so `parallel()` over
 *   `agent()` runs concurrently under a host that admits one submission at a time.
 * - An unlabelled agent's node id follows its position in the program (per-`parallel()`-item
 *   scope), not completion order. A replay, which returns saved results instantly, reassigns the
 *   same ids and so reuses every saved result.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/advanced/public.js"
import { DynamicWorkflowExecutor, DynamicWorkflowLimitError, type DynamicWorkflowContext, type DynamicWorkflowHost, type DynamicWorkflowScript } from "../src/workflow/dynamic.js"
import { DynamicWorkflowProcessExecutor } from "../src/workflow/dynamic-process.js"
import { InMemoryDynamicWorkflowReplayStore, fingerprintDynamicWorkflowInvocation } from "../src/workflow/dynamic-replay.js"

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

/** A host that runs every node of a submission concurrently, each after its own delay. */
function delayedHost(delays: Record<string, number>, executed: string[]): DynamicWorkflowHost {
  return {
    async runWorkflow(spec) {
      const settled = await Promise.all(spec.nodes.map(async node => {
        const goal = typeof node.task === "string" ? node.task : node.task.goal
        executed.push(`${node.nodeId}=${goal}`)
        await sleep(delays[goal] ?? 1)
        return { nodeId: node.nodeId!, content: `out:${goal}` }
      }))
      return {
        nodeOutcomes: settled.map(({ nodeId, content }) => ({
          nodeId,
          status: "completed" as const,
          output: { role: "assistant" as const, content },
        })),
        outputs: Object.fromEntries(settled.map(({ nodeId, content }) => [nodeId, content])),
      }
    },
  }
}

/** Two items, each a draft then a review. Item A has an extra first step, so its steps lag B's. */
const draftAndReview = async (ctx: DynamicWorkflowContext) => ctx.parallel(["A", "B"], async item => {
  if (item === "A") await ctx.agent("outline A")
  const draft = await ctx.agent(`draft ${item}`)
  const review = await ctx.agent(`review ${item}`)
  return `${draft?.text}|${review?.text}`
})

describe("dynamic workflow concurrency", () => {
  it("runs parallel() over agent() concurrently under RuntimeRunner", async () => {
    let active = 0
    let peak = 0
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: {
        async run(ctx: { manifest: { agent_id: string } }) {
          active += 1
          peak = Math.max(peak, active)
          await sleep(20)
          active -= 1
          return {
            agentId: ctx.manifest.agent_id,
            result: { termination: "completed", finalMessage: { role: "assistant", content: "ok", toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 },
          }
        },
      },
    } as never)

    const run = await runner.runDynamicWorkflow(
      async ctx => ctx.parallel(["a", "b", "c", "d"], item => ctx.agent(`task ${item}`)),
      { sessionId: "concurrent-parallel" },
    )

    expect(run.value.map(result => result?.text)).toEqual(["ok", "ok", "ok", "ok"])
    expect(peak).toBe(4)
  })

  it("never puts more than maxConcurrentAgents calls in one batch", async () => {
    const sizes: number[] = []
    const host: DynamicWorkflowHost = {
      async runWorkflow(spec) {
        sizes.push(spec.nodes.length)
        return delayedHost({}, []).runWorkflow(spec)
      },
    }
    await new DynamicWorkflowExecutor(host, { limits: { maxConcurrentAgents: 2 } })
      .run(ctx => Promise.all(["a", "b", "c", "d", "e"].map(item => ctx.agent(`task ${item}`))))

    expect(sizes).toEqual([2, 2, 1])
  })
})

describe("dynamic workflow per-call failures", () => {
  const settle = (calls: Array<Promise<unknown>>) => Promise.allSettled(calls).then(results =>
    results.map(result => result.status === "fulfilled" ? "ok" : (result.reason as Error).constructor.name))

  it("fails only the calls past maxAgentsPerRun in a batch", async () => {
    const executed: string[] = []
    const run = await new DynamicWorkflowExecutor(delayedHost({}, executed), { limits: { maxAgentsPerRun: 2 } })
      .run(ctx => settle(["a", "b", "c"].map(item => ctx.agent(`task ${item}`))))

    expect(run.value).toEqual(["ok", "ok", DynamicWorkflowLimitError.name])
    expect(executed).toHaveLength(2)
  })

  it("fails only the calls the host was running when it throws", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    // An interrupted run: one saved result, then the script stopped.
    await expect(new DynamicWorkflowExecutor(delayedHost({}, []), { runId: "host-error", replayStore: store })
      .run(async ctx => { await ctx.agent("saved", { label: "kept" }); throw new Error("stopped") }))
      .rejects.toThrow("stopped")
    const failing: DynamicWorkflowHost = { async runWorkflow() { throw new Error("host down") } }

    const run = await new DynamicWorkflowExecutor(failing, { runId: "host-error", replayStore: store })
      .run(ctx => settle([ctx.agent("saved", { label: "kept" }), ctx.agent("fresh", { label: "new" })]))

    expect(run.value).toEqual(["ok", "Error"])
  })

  it("keeps parallelAgents all-or-nothing on the limit", async () => {
    const executed: string[] = []
    await expect(new DynamicWorkflowExecutor(delayedHost({}, executed), { limits: { maxAgentsPerRun: 2 } })
      .run(ctx => ctx.parallelAgents(["a", "b", "c"], item => ({ prompt: `task ${item}` }))))
      .rejects.toBeInstanceOf(DynamicWorkflowLimitError)
    expect(executed).toEqual([])
  })
})

describe("dynamic workflow replay identity", () => {
  it("assigns the same node ids whatever order siblings finish in", async () => {
    const mapping = async (delays: Record<string, number>) => {
      const executed: string[] = []
      await new DynamicWorkflowExecutor(delayedHost(delays, executed), {
        runId: "ids",
        replayStore: new InMemoryDynamicWorkflowReplayStore(),
      }).run(draftAndReview)
      return executed.sort()
    }

    const slowA = await mapping({ "outline A": 40, "draft A": 40 })
    const slowB = await mapping({ "draft B": 40, "review B": 40 })

    expect(slowA).toEqual(slowB)
    expect(slowA).toEqual([
      "dynamic-agent-p0.0.0=outline A",
      "dynamic-agent-p0.0.1=draft A",
      "dynamic-agent-p0.0.2=review A",
      "dynamic-agent-p0.1.0=draft B",
      "dynamic-agent-p0.1.1=review B",
    ])
  })

  it("replays a completed run without executing any agent again", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    const firstExecuted: string[] = []
    const first = await new DynamicWorkflowExecutor(
      delayedHost({ "outline A": 30, "draft B": 1, "review B": 1 }, firstExecuted),
      { runId: "replayed", replayStore: store },
    ).run(draftAndReview)
    expect(firstExecuted).toHaveLength(5)

    const replayExecuted: string[] = []
    const replay = await new DynamicWorkflowExecutor(delayedHost({}, replayExecuted), {
      runId: "replayed",
      replayStore: store,
    }).run(draftAndReview)

    expect(replayExecuted).toEqual([])
    expect(replay.value).toEqual(first.value)
    expect(replay.progress.agentsReused).toBe(5)
  })

  it("resumes a run recorded before scoped ids with its original call-order ids", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    // Seed a real run for its fingerprints, then rewrite it the way an earlier SDK saved it:
    // completed, no `nodeIds` marker, ids from one global call-order counter.
    await new DynamicWorkflowExecutor(delayedHost({}, []), { runId: "legacy", replayStore: store })
      .run(async ctx => ctx.agent("seed"))
    const seeded = (await store.loadRun("legacy"))!
    await store.saveRun("legacy", {
      ...seeded,
      nodeIds: undefined,
      status: "completed",
      records: ["draft A", "draft B"].map((prompt, index) => ({
        nodeId: `dynamic-agent-${index}`,
        promptFingerprint: fingerprintDynamicWorkflowInvocation(prompt, {}),
        text: `saved:${prompt}`,
        status: "completed" as const,
        termination: "completed",
      })),
    })

    const executed: string[] = []
    const resumed = await new DynamicWorkflowExecutor(delayedHost({}, executed), { runId: "legacy", replayStore: store })
      .run(ctx => ctx.parallel(["A", "B"], async item => (await ctx.agent(`draft ${item}`))?.text))

    expect(executed).toEqual([])
    expect(resumed.value).toEqual(["saved:draft A", "saved:draft B"])
    expect((await store.loadRun("legacy"))?.nodeIds).toBeUndefined()
  })

  it("scopes a process-isolated script's parallel() items the same way", async () => {
    const script: DynamicWorkflowScript = {
      meta: { name: "process-parallel", description: "Process parallel", phases: [] },
      source: `
        return await workflow.parallel(["A", "B"], async item => {
          const draft = await workflow.agent("draft " + item);
          const review = await workflow.agent("review " + item);
          return draft.text + "|" + review.text;
        });
      `,
    }
    const executed: string[] = []
    const run = await new DynamicWorkflowProcessExecutor(delayedHost({ "draft A": 40 }, executed))
      .runScript(script, { runId: "process-scoped", trust: "untrusted" })

    expect(run.value).toEqual(["out:draft A|out:review A", "out:draft B|out:review B"])
    expect(executed.sort()).toEqual([
      "dynamic-agent-p0.0.0=draft A",
      "dynamic-agent-p0.0.1=review A",
      "dynamic-agent-p0.1.0=draft B",
      "dynamic-agent-p0.1.1=review B",
    ])
  })
})
