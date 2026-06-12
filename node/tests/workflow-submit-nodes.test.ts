import { getKernel } from "../src/kernel.js"
import { submitWorkflowNodesToKernel } from "../src/types/agent.js"
import type { WorkflowSpec, WorkflowSpawnInfo } from "../src/types/agent.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return JSON.parse(rt.step(JSON.stringify({ version: 1, event }))) as {
    actions: Array<Record<string, unknown>>
    observations: Array<{ kind: string; nodes?: WorkflowSpawnInfo[]; completed?: string[]; failed?: string[] }>
  }
}

function complete(rt: { step(json: string): string }, agentId: string) {
  return step(rt, {
    kind: "sub_agent_completed",
    result: {
      agent_id: agentId,
      result: {
        termination: "completed",
        final_message: { role: "assistant", content: "ok", tool_calls: [] },
        turns_used: 1,
        total_tokens_used: 1,
      },
    },
  })
}

describe("submitWorkflowNodesToKernel", () => {
  it("maps host nodes → the submit_workflow_nodes kernel event (string-task shorthand + defaults)", () => {
    const event = submitWorkflowNodesToKernel([{ task: "more", role: "implement" }])
    expect(event).toEqual({
      kind: "submit_workflow_nodes",
      nodes: [
        { task: { goal: "more", criteria: [] }, role: "implement", isolation: "shared", context_inheritance: "none" },
      ],
    })
  })

  it("carries trust + batch-relative deps through to the kernel shape", () => {
    const event = submitWorkflowNodesToKernel([
      { task: { goal: "extract", criteria: [] }, role: "explore", trust: "quarantined" },
      { task: "verify", role: "verify", dependsOn: [0] },
    ]) as { nodes: Array<Record<string, unknown>> }
    expect(event.nodes[0]).toMatchObject({ trust: "quarantined", role: "explore" })
    expect(event.nodes[1]).toMatchObject({ depends_on: [0], role: "verify" })
  })
})

describe("submit_workflow_nodes over the kernel ABI", () => {
  it("appends a node to a running workflow and keeps it alive until the appended node completes", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    // A single-node workflow: wf-node0 spawns first.
    const spec: WorkflowSpec = { nodes: [{ task: "root", role: "implement" }] }
    const loaded = step(rt, {
      kind: "load_workflow",
      spec: { nodes: [{ task: { goal: "root", criteria: [] }, role: "implement", isolation: "shared", context_inheritance: "none" }] },
      parent_session_id: "sess",
    })
    expect(
      loaded.observations.some(o => o.kind === "workflow_batch_spawned" && o.nodes?.some(n => n.agent_id === "wf-node0")),
    ).toBe(true)
    void spec

    // Submit a node over the ABI while wf-node0 runs → it spawns immediately as wf-node1.
    const submitted = step(rt, submitWorkflowNodesToKernel([{ task: "more", role: "implement" }]))
    expect(
      submitted.observations.some(
        o => o.kind === "workflow_batch_spawned" && o.nodes?.some(n => n.agent_id === "wf-node1" && n.goal === "more"),
      ),
    ).toBe(true)

    // The workflow finishes only after BOTH the root and the submitted node complete.
    const afterRoot = complete(rt, "wf-node0")
    expect(afterRoot.observations.some(o => o.kind === "workflow_completed")).toBe(false)
    const done = complete(rt, "wf-node1")
    const completed = done.observations.find(o => o.kind === "workflow_completed")
    expect(completed?.completed?.sort()).toEqual(["wf-node0", "wf-node1"])
  })
})
