import { schedulerPolicyToKernel } from "../../src/runtime/runner.js"
import { startWorkflowTool, submitWorkflowNodesTool, workflowNodeSpecToKernel } from "../../src/types/agent.js"

describe("scheduler policy ABI", () => {
  it("lowers every deterministic ordering weight", () => {
    expect(schedulerPolicyToKernel({
      criticalPathWeight: 1_000_000,
      fanoutWeight: 10_000,
      ageWeight: 1_000,
      tokenCostWeight: 1,
      deadlineWeight: 7,
      processPriorityWeight: 6,
      resourcePressureWeight: 5,
      budgetPressureWeight: 4,
    })).toEqual({
      critical_path_weight: 1_000_000,
      fanout_weight: 10_000,
      age_weight: 1_000,
      token_cost_weight: 1,
      deadline_weight: 7,
      process_priority_weight: 6,
      resource_pressure_weight: 5,
      budget_pressure_weight: 4,
    })
  })

  it("rejects the retired wall-budget field", () => {
    expect(() => schedulerPolicyToKernel({
      criticalPathWeight: 1,
      fanoutWeight: 1,
      ageWeight: 1,
      tokenCostWeight: 1,
      deadlineWeight: 0,
      processPriorityWeight: 0,
      resourcePressureWeight: 0,
      budgetPressureWeight: 0,
      maxWallMs: 1234,
    } as any)).toThrow(/unknown scheduler policy field.*maxWallMs/)
  })

  it("keeps optional weights absent when a policy does not opt in", () => {
    expect(schedulerPolicyToKernel({
      criticalPathWeight: 1,
      fanoutWeight: 1,
      ageWeight: 1,
      tokenCostWeight: 1,
    })).toEqual({
      critical_path_weight: 1,
      fanout_weight: 1,
      age_weight: 1,
      token_cost_weight: 1,
    })
  })

  it("lowers host-only per-node scheduling factors with stable integer validation", () => {
    expect(workflowNodeSpecToKernel({
      task: "ship", role: "implement",
      schedulingFactors: { deadlineUrgency: 4, processPriority: 3, resourcePressure: 2, budgetPressure: 1 },
    })).toMatchObject({
      scheduling_factors: { deadline_urgency: 4, process_priority: 3, resource_pressure: 2, budget_pressure: 1 },
    })
    expect(() => workflowNodeSpecToKernel({
      task: "ship", role: "implement", schedulingFactors: { deadlineUrgency: -1 },
    })).toThrow(/non-negative safe integer/)
  })

  it("does not expose host scheduling factors in model workflow tools", () => {
    const startNode = JSON.parse(startWorkflowTool.parameters).properties.spec.properties.nodes.items.properties
    const submitNode = JSON.parse(submitWorkflowNodesTool.parameters).properties.nodes.items.properties
    expect(startNode).not.toHaveProperty("schedulingFactors")
    expect(submitNode).not.toHaveProperty("schedulingFactors")
  })
})
