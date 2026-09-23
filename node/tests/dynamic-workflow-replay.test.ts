import { DynamicWorkflowExecutor } from "../src/workflow/dynamic.js"
import { InMemoryDynamicWorkflowReplayStore } from "../src/workflow/dynamic-replay.js"
import type { DynamicWorkflowHost } from "../src/workflow/dynamic.js"

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
})

