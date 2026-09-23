import { workflowNodeSpecToKernel, workflowSpecToKernel } from "../src/types/agent.js"
import { workflowBudgetFromKernel, workflowSpawnNodeFromKernel } from "../src/runtime/kernel-step.js"

describe("workflow host-to-kernel boundary", () => {
  it("projects node fields and control-flow kind into the kernel shape", () => {
    expect(workflowNodeSpecToKernel({
      nodeId: "step-1",
      agent: "worker",
      task: "implement",
      role: "implement",
      isolation: "read_only",
      contextInheritance: "system_only",
      modelHint: "fast",
      tokenBudget: 100,
      dependsOn: [0],
      depPolicy: "accept_partial",
      loop: { maxIters: 2 },
    })).toEqual({
      task: { goal: "implement", criteria: [] },
      role: "implement",
      isolation: "read_only",
      context_inheritance: "system_only",
      model_hint: "fast",
      kind: { type: "loop", max_iters: 2 },
      token_budget: 100,
      depends_on: [0],
      dep_policy: "accept_partial",
    })
  })

  it("composes a workflow root from the same node projection", () => {
    expect(workflowSpecToKernel({
      nodes: [{ task: "plan", role: "plan" }],
    })).toEqual({
      nodes: [{
        task: { goal: "plan", criteria: [] },
        role: "plan",
        isolation: "shared",
        context_inheritance: "none",
        dep_policy: "all_success",
      }],
    })
  })

  it("projects kernel spawn bookkeeping into the host runner shape", () => {
    expect(workflowSpawnNodeFromKernel({
      agent_id: "wf-node0",
      goal: "implement",
      role: "worker",
      isolation: "shared",
      context_inheritance: "none",
      task_id: "task-0",
      attempt_id: "attempt-0",
      launch_token: "opaque",
      node_id: "node-0",
      reducer: "join",
      input_agent_ids: ["wf-node1"],
      token_budget: 100,
    })).toEqual({
      agent_id: "wf-node0",
      goal: "implement",
      role: "worker",
      isolation: "shared",
      context_inheritance: "none",
      reducer: "join",
      input_agent_ids: ["wf-node1"],
      token_budget: 100,
    })
  })

  it("keeps the kernel budget snapshot typed at the host boundary", () => {
    expect(workflowBudgetFromKernel({ nodes_remaining: 2, max_total_tokens: "5000", max_concurrency: 2 })).toEqual({
      nodes_remaining: 2,
      max_total_tokens: "5000",
      max_concurrency: 2,
    })
  })
})
