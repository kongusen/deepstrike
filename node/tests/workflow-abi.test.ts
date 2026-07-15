import { getKernel } from "../src/kernel.js"
import {
  workflowSpecToKernel,
  workflowNodeToSpec,
  fanoutSynthesize,
  generateAndFilter,
  verifyRules,
} from "../src/types/agent.js"
import type { WorkflowSpec, WorkflowSpawnInfo } from "../src/types/agent.js"
import { buildWorkflowNodeCompletedEvent, recoverCompletedWorkflowNodes } from "../src/runtime/session-repair.js"
import { stepKernelV2WithHostEffects } from "./helpers/kernel-v2.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return stepKernelV2WithHostEffects(rt as never, event) as {
    actions: Array<Record<string, unknown>>
    observations: Array<{
      kind: string
      nodes?: WorkflowSpawnInfo[]
      completed?: string[]
      failed?: string[]
    }>
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

describe("workflowSpecToKernel", () => {
  it("maps camelCase host spec → snake_case kernel JSON, with string-task shorthand", () => {
    const spec: WorkflowSpec = {
      nodes: [
        { task: "w0", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
        { task: { goal: "synth", criteria: ["merge"] }, role: "plan", dependsOn: [0] },
      ],
    }
    const k = workflowSpecToKernel(spec) as { nodes: Array<Record<string, unknown>> }
    expect(k.nodes[0]).toEqual({
      task: { goal: "w0", criteria: [] },
      role: "explore",
      isolation: "read_only",
      context_inheritance: "system_only",
    })
    // node 2: defaults applied (isolation/context_inheritance always emitted), deps + criteria kept
    expect(k.nodes[1]).toEqual({
      task: { goal: "synth", criteria: ["merge"] },
      role: "plan",
      isolation: "shared",
      context_inheritance: "none",
      depends_on: [0],
    })
    // string-task shorthand still yields an empty criteria array
    expect((k.nodes[0] as { task: { criteria: unknown } }).task.criteria).toEqual([])
  })
})

describe("workflow templates", () => {
  it("fanoutSynthesize: parallel explore workers → plan synthesizer", () => {
    const spec = fanoutSynthesize(["a", "b", "c"], "merge")
    expect(spec.nodes).toHaveLength(4)
    expect(spec.nodes[0]).toMatchObject({ role: "explore", isolation: "read_only", contextInheritance: "system_only" })
    expect(spec.nodes[3]).toMatchObject({ role: "plan", dependsOn: [0, 1, 2] })
  })

  it("generateAndFilter: implement generators → verify filter", () => {
    const spec = generateAndFilter(["x", "y"], "dedupe")
    expect(spec.nodes).toHaveLength(3)
    expect(spec.nodes[0]).toMatchObject({ role: "implement" })
    expect(spec.nodes[2]).toMatchObject({ role: "verify", contextInheritance: "none", dependsOn: [0, 1] })
  })

  it("verifyRules: bias-resistant verifiers + skeptic", () => {
    const spec = verifyRules(["rule1", "rule2"], "skeptic")
    expect(spec.nodes).toHaveLength(3)
    for (const n of spec.nodes.slice(0, 2)) {
      expect(n).toMatchObject({ role: "verify", isolation: "read_only", contextInheritance: "none" })
      expect(n.dependsOn).toBeUndefined()
    }
    expect(spec.nodes[2].dependsOn).toEqual([0, 1])
    // no skeptic → just verifiers
    expect(verifyRules(["only"]).nodes).toHaveLength(1)
  })
})

describe("workflowNodeToSpec", () => {
  it("builds a sub-agent run spec from a kernel spawn descriptor", () => {
    const node: WorkflowSpawnInfo = {
      agent_id: "wf-node0",
      goal: "do it",
      role: "implement",
      isolation: "worktree",
      context_inheritance: "full",
    }
    const spec = workflowNodeToSpec(node, "parent")
    expect(spec.goal).toBe("do it")
    expect(spec.role).toBe("implement")
    expect(spec.isolation).toBe("worktree")
    expect(spec.identity).toEqual({
      agentId: "wf-node0",
      sessionId: "parent-wf-node0",
      isSubAgent: true,
      parentSessionId: "parent",
    })
  })
})

describe("LoadWorkflow ABI drives the DAG end-to-end", () => {
  it("fanout: workers batch → synth batch → workflow_completed", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    const spec: WorkflowSpec = {
      nodes: [
        { task: "w0", role: "explore" },
        { task: "w1", role: "explore" },
        { task: "synth", role: "plan", dependsOn: [0, 1] },
      ],
    }
    const loaded = step(rt, {
      kind: "load_workflow",
      spec: workflowSpecToKernel(spec),
      parent_session_id: "sess",
    })

    // First batch carries both workers' goals (so the SDK can run them).
    const batch1 = loaded.observations.find(o => o.kind === "workflow_batch_spawned")
    expect(batch1?.nodes?.map(n => n.goal).sort()).toEqual(["w0", "w1"])
    expect(batch1?.nodes?.map(n => n.agent_id).sort()).toEqual(["wf-node0", "wf-node1"])
    expect(loaded.actions).toHaveLength(0) // suspended on the batch

    // First worker done → still suspended, no new batch.
    const afterW0 = complete(rt, "wf-node0")
    expect(afterW0.observations.some(o => o.kind === "workflow_batch_spawned")).toBe(false)

    // Second worker done → synth batch emitted.
    const afterW1 = complete(rt, "wf-node1")
    const batch2 = afterW1.observations.find(o => o.kind === "workflow_batch_spawned")
    expect(batch2?.nodes?.map(n => n.agent_id)).toEqual(["wf-node2"])
    expect(batch2?.nodes?.[0]?.goal).toBe("synth")

    // Synth done → workflow completes, parent resumes.
    const afterSynth = complete(rt, "wf-node2")
    const completed = afterSynth.observations.find(o => o.kind === "workflow_completed")
    expect(completed?.completed?.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
    expect(afterSynth.actions[0]?.kind).toBe("call_provider")
  })
})
import { getKernel } from "../src/kernel.js"
import { buildWorkflowNodeCompletedEvent, recoverCompletedWorkflowNodes } from "../src/runtime/session-repair.js"

describe("resume persistence", () => {
  it("buildWorkflowNodeCompletedEvent builds a valid SessionEvent", () => {
    const event = buildWorkflowNodeCompletedEvent({
      turn: 5,
      agentId: "wf-node3",
      termination: "completed",
    })
    expect(event.kind).toBe("workflow_node_completed")
    expect(event.turn).toBe(5)
    expect(event.agent_id).toBe("wf-node3")
    expect(event.termination).toBe("completed")
    // category and primitive are added by the logging layer (withCategory)
  })

  it("recoverCompletedWorkflowNodes extracts completed records with their control signals", () => {
    const events = [
      { seq: 0, event: { kind: "run_started", run_id: "s1", goal: "test", criteria: [] } },
      { seq: 1, event: buildWorkflowNodeCompletedEvent({ turn: 1, agentId: "wf-node0", termination: "completed", classifyBranch: "a", output: "picked a" }) },
      { seq: 2, event: buildWorkflowNodeCompletedEvent({ turn: 2, agentId: "wf-node1", termination: "failed" }) },
      { seq: 3, event: buildWorkflowNodeCompletedEvent({ turn: 3, agentId: "wf-node2", termination: "completed" }) },
      { seq: 4, event: { kind: "run_terminal", reason: "done", turns_used: 3, total_tokens: 10 } },
    ]
    const completed = recoverCompletedWorkflowNodes(events)
    // W-1: records (not bare ids) — signals + output ride along for faithful control-flow replay.
    expect(completed).toEqual([
      { agentId: "wf-node0", classifyBranch: "a", output: "picked a" },
      { agentId: "wf-node2" },
    ])
  })

  it("recoverCompletedWorkflowNodes returns empty for empty stream", () => {
    expect(recoverCompletedWorkflowNodes([])).toEqual([])
    expect(recoverCompletedWorkflowNodes([{ seq: 0, event: { kind: "run_started", run_id: "s1", goal: "x", criteria: [] } }])).toEqual([])
  })

  it("kernel resumes workflow from completed nodes", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    const spec: WorkflowSpec = {
      nodes: [
        { task: "w0", role: "explore" },
        { task: "w1", role: "explore" },
        { task: "synth", role: "plan", dependsOn: [0, 1] },
      ],
    }

    // Resume with node0 already completed.
    const loaded = step(rt, {
      kind: "load_workflow",
      spec: workflowSpecToKernel(spec),
      parent_session_id: "sess",
      resumed_completed: ["wf-node0"],
    })

    // Only node1 is in the ready batch; node0 is skipped.
    const batch1 = loaded.observations.find(o => o.kind === "workflow_batch_spawned")
    expect(batch1?.nodes?.map(n => n.agent_id)).toEqual(["wf-node1"])
    expect(batch1?.nodes?.length).toBe(1)

    // Complete node1 → synth batch appears.
    const afterW1 = complete(rt, "wf-node1")
    const batch2 = afterW1.observations.find(o => o.kind === "workflow_batch_spawned")
    expect(batch2?.nodes?.map(n => n.agent_id)).toEqual(["wf-node2"])

    // Synth done → workflow completes with all three nodes.
    const afterSynth = complete(rt, "wf-node2")
    const completed = afterSynth.observations.find(o => o.kind === "workflow_completed")
    expect(completed?.completed?.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
  })

  it("kernel resumes with all nodes already completed", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    const spec: WorkflowSpec = {
      nodes: [
        { task: "w0", role: "explore" },
        { task: "synth", role: "plan", dependsOn: [0] },
      ],
    }

    // Both nodes already completed → immediate workflow_completed.
    const loaded = step(rt, {
      kind: "load_workflow",
      spec: workflowSpecToKernel(spec),
      parent_session_id: "sess",
      resumed_completed: ["wf-node0", "wf-node1"],
    })

    const completed = loaded.observations.find(o => o.kind === "workflow_completed")
    expect(completed?.completed?.sort()).toEqual(["wf-node0", "wf-node1"])
    expect(loaded.actions[0]?.kind).toBe("call_provider")
  })
})
