import {
  workflowSpecToKernel,
  workflowNodeToSpec,
  fanoutSynthesize,
  generateAndFilter,
  verifyRules,
} from "../src/types/agent.js"
import type { WorkflowSpec, WorkflowSpawnInfo } from "../src/types/agent.js"
import { buildWorkflowNodeCompletedEvent } from "../src/runtime/session-repair.js"

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
      dep_policy: "all_success",
    })
    // node 2: defaults applied (isolation/context_inheritance always emitted), deps + criteria kept
    expect(k.nodes[1]).toEqual({
      task: { goal: "synth", criteria: ["merge"] },
      role: "plan",
      isolation: "shared",
      context_inheritance: "none",
      depends_on: [0],
      dep_policy: "all_success",
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

describe("workflow audit persistence", () => {
  it("buildWorkflowNodeCompletedEvent builds a valid SessionEvent", () => {
    const event = buildWorkflowNodeCompletedEvent({
      turn: 5,
      agentId: "wf-node3",
      status: "completed",
      termination: "completed",
    })
    expect(event.kind).toBe("workflow_node_completed")
    expect(event.turn).toBe(5)
    expect(event.agent_id).toBe("wf-node3")
    expect(event.termination).toBe("completed")
    // Category and primitive are added by the logging layer; this event is audit-only.
  })
})
