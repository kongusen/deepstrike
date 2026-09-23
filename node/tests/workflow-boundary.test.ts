import { workflowNodeSpecToKernel, workflowSpecToKernel } from "../src/types/agent.js"

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
})
