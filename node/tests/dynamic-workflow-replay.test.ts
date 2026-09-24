import { DynamicWorkflowExecutor, DynamicWorkflowNothingToResumeError, DynamicWorkflowReplayMismatchError, dynamicAgentTask } from "../src/workflow/dynamic.js"
import { FileDynamicWorkflowReplayStore, InMemoryDynamicWorkflowReplayStore } from "../src/workflow/dynamic-replay.js"
import type { DynamicWorkflowHost } from "../src/workflow/dynamic.js"
import { mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

function hostWithCalls(calls: string[]): DynamicWorkflowHost {
  return {
    async runWorkflow(spec) {
      const node = spec.nodes[0]
      const goal = typeof node.task === "string" ? node.task : node.task.goal
      calls.push(goal)
      return {
        nodeOutcomes: [{ nodeId: node.nodeId!, status: "completed", output: { role: "assistant", content: `done:${goal}` } }],
        outputs: { [node.nodeId!]: `done:${goal}` },
      }
    },
  }
}

function batchHostWithCalls(batches: string[][]): DynamicWorkflowHost {
  return {
    async runWorkflow(spec) {
      const goals = spec.nodes.map(node => typeof node.task === "string" ? node.task : node.task.goal)
      batches.push(goals)
      return {
        nodeOutcomes: spec.nodes.map(node => ({
          nodeId: node.nodeId!,
          status: "completed" as const,
          output: { role: "assistant" as const, content: `done:${typeof node.task === "string" ? node.task : node.task.goal}` },
        })),
        outputs: Object.fromEntries(spec.nodes.map(node => {
          const goal = typeof node.task === "string" ? node.task : node.task.goal
          return [node.nodeId!, `done:${goal}`]
        })),
      }
    },
  }
}

describe("dynamic workflow replay", () => {
  it("reuses a completed invocation only when its prompt fingerprint is unchanged", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    const calls: string[] = []
    const first = new DynamicWorkflowExecutor(hostWithCalls(calls), { runId: "replay-1", replayStore: store })
    await first.run(ctx => ctx.agent("inspect", { label: "inspect" }))

    const second = new DynamicWorkflowExecutor(hostWithCalls(calls), { runId: "replay-1", replayStore: store })
    const reused = await second.run(ctx => ctx.agent("inspect", { label: "inspect" }))
    expect(calls).toEqual(["inspect"])
    expect(reused.progress.agentsReused).toBe(1)

    const changed = new DynamicWorkflowExecutor(hostWithCalls(calls), { runId: "replay-1", replayStore: store })
    await changed.run(ctx => ctx.agent("inspect again", { label: "inspect" }))
    expect(calls).toEqual(["inspect", "inspect again"])
  })

  it("invalidates sequential replay after the first changed invocation", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    const calls: string[] = []
    const run = (first: string, second: string) => new DynamicWorkflowExecutor(hostWithCalls(calls), {
      runId: "sequential-suffix-1",
      replayStore: store,
    }).run(async ctx => [await ctx.agent(first, { label: "first" }), await ctx.agent(second, { label: "second" })])

    await run("a", "b")
    await run("changed", "b")
    expect(calls).toEqual(["a", "b", "changed", "b"])
  })

  it("rejects replay when run inputs or artifact identity change", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    await new DynamicWorkflowExecutor(hostWithCalls([]), {
      runId: "input-mismatch-1",
      replayStore: store,
      args: { target: "a" },
      artifactDigest: "sha-a",
    }).run(ctx => ctx.agent("inspect", { label: "inspect" }))

    await expect(new DynamicWorkflowExecutor(hostWithCalls([]), {
      runId: "input-mismatch-1",
      replayStore: store,
      args: { target: "b" },
      artifactDigest: "sha-a",
    }).run(ctx => ctx.agent("inspect", { label: "inspect" }))).rejects.toBeInstanceOf(DynamicWorkflowReplayMismatchError)

    await expect(new DynamicWorkflowExecutor(hostWithCalls([]), {
      runId: "input-mismatch-1",
      replayStore: store,
      args: { target: "a" },
      artifactDigest: "sha-b",
    }).run(ctx => ctx.agent("inspect", { label: "inspect" }))).rejects.toBeInstanceOf(DynamicWorkflowReplayMismatchError)
  })

  it("persists failed and cancelled invocation records without replaying them", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    await store.save("failed-1", {
      nodeId: "failed-node",
      promptFingerprint: "f".repeat(64),
      text: "partial",
      status: "failed",
      termination: "provider_error",
      error: "provider unavailable",
    })
    await store.save("failed-1", {
      nodeId: "cancelled-node",
      promptFingerprint: "c".repeat(64),
      text: "",
      status: "cancelled",
      termination: "workflow_rejected",
    })

    await expect(store.find("failed-1", "failed-node", "f".repeat(64))).resolves.toBeUndefined()
    await expect(store.find("failed-1", "cancelled-node", "c".repeat(64))).resolves.toBeUndefined()
    await expect(store.loadRun("failed-1")).resolves.toMatchObject({ records: [
      expect.objectContaining({ status: "failed", error: "provider unavailable" }),
      expect.objectContaining({ status: "cancelled" }),
    ] })
  })

  it("rejects a completed artifact run when its saved invocation result is missing", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    await store.saveRun("missing-result-1", {
      version: 2,
      runId: "missing-result-1",
      status: "completed",
      artifact: { name: "audit", digest: "digest-a", meta: { name: "audit", description: "Audit" } },
      events: [],
      records: [],
    })
    await expect(new DynamicWorkflowExecutor(hostWithCalls([]), {
      runId: "missing-result-1",
      artifactDigest: "digest-a",
      artifactSnapshot: { name: "audit", digest: "digest-a", meta: { name: "audit", description: "Audit" } },
      replayStore: store,
    }).run(ctx => ctx.agent("inspect", { label: "inspect" }))).rejects.toBeInstanceOf(DynamicWorkflowNothingToResumeError)
  })

  it("returns lifecycle events for approved runs and phases", async () => {
    const events: string[] = []
    const executor = new DynamicWorkflowExecutor(hostWithCalls([]), {
      runId: "lifecycle-1",
      onLifecycleEvent: event => events.push(event.kind),
    })
    const run = await executor.run(async ctx => ctx.phase("inspect", async () => {
      ctx.log("starting")
      return (await ctx.agent("inspect", { label: "inspect" }))?.text
    }))

    expect(run.events.map(event => event.kind)).toEqual([
      "run_started",
      "approval_requested",
      "approval_resolved",
      "phase_started",
      "log",
      "agent_started",
      "agent_completed",
      "phase_completed",
      "run_completed",
    ])
    expect(events).toEqual(run.events.map(event => event.kind))
  })

  it("persists lifecycle events exactly once in a file replay snapshot", async () => {
    const root = await mkdtemp(join(tmpdir(), "dynamic-wf-events-"))
    try {
      const store = new FileDynamicWorkflowReplayStore({ rootDir: root })
      const run = await new DynamicWorkflowExecutor(hostWithCalls([]), {
        runId: "events-1",
        replayStore: store,
      }).run(ctx => ctx.phase("inspect", async () => {
        ctx.log("starting")
        return (await ctx.agent("inspect", { label: "inspect" }))?.text
      }))
      const persisted = await store.loadRun("events-1")
      expect(persisted?.events).toEqual(run.events)
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  it("reuses unchanged fanout items and submits only the changed suffix", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    const batches: string[][] = []
    const first = new DynamicWorkflowExecutor(batchHostWithCalls(batches), { runId: "fanout-1", replayStore: store })
    await first.run(ctx => ctx.parallelAgents(["a", "b", "c"], (item, index) =>
      dynamicAgentTask(`task:${item}`, { label: `slot-${index}` }),
    ))

    const second = new DynamicWorkflowExecutor(batchHostWithCalls(batches), { runId: "fanout-1", replayStore: store })
    const replayed = await second.run(ctx => ctx.parallelAgents(["a", "changed", "c"], (item, index) =>
      dynamicAgentTask(`task:${item}`, { label: `slot-${index}` }),
    ))

    expect(batches).toEqual([["task:a", "task:b", "task:c"], ["task:changed", "task:c"]])
    expect(replayed.value.map(result => result?.text)).toEqual(["done:task:a", "done:task:changed", "done:task:c"])
    expect(replayed.progress.agentsReused).toBe(1)
    expect(replayed.progress.agentsStarted).toBe(2)
  })

  it("does not consume the agent limit for a replay hit", async () => {
    const store = new InMemoryDynamicWorkflowReplayStore()
    const calls: string[] = []
    const executor = new DynamicWorkflowExecutor(hostWithCalls(calls), {
      runId: "limit-replay-1",
      replayStore: store,
      limits: { maxAgentsPerRun: 1 },
    })

    const run = await executor.run(async ctx => [
      await ctx.agent("inspect", { label: "inspect" }),
      await ctx.agent("inspect", { label: "inspect" }),
    ])

    expect(calls).toEqual(["inspect"])
    expect(run.progress.agentsReused).toBe(1)
    expect(run.progress.agentsStarted).toBe(1)
  })

  it("serializes concurrent file replay writes without losing completed items", async () => {
    const root = await mkdtemp(join(tmpdir(), "dynamic-wf-replay-"))
    try {
      const store = new FileDynamicWorkflowReplayStore({ rootDir: root })
      await Promise.all(["a", "b", "c"].map((nodeId, index) => store.save("run-1", {
        nodeId,
        promptFingerprint: `${String(index).repeat(64)}`,
        text: `done:${nodeId}`,
        status: "completed",
      })))

      await expect(store.find("run-1", "a", "0".repeat(64))).resolves.toMatchObject({ text: "done:a" })
      await expect(store.find("run-1", "b", "1".repeat(64))).resolves.toMatchObject({ text: "done:b" })
      await expect(store.find("run-1", "c", "2".repeat(64))).resolves.toMatchObject({ text: "done:c" })
      await expect(readFile(join(root, "run-1.json"), "utf8")).resolves.toContain('"version": 2')
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })
})
