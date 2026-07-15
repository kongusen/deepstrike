import { schedulerPolicyToKernel } from "../../src/runtime/runner.js"

describe("scheduler policy ABI", () => {
  it("lowers only deterministic ordering weights", () => {
    expect(schedulerPolicyToKernel({
      version: 1,
      criticalPathWeight: 1_000_000,
      fanoutWeight: 10_000,
      ageWeight: 1_000,
      tokenCostWeight: 1,
    })).toEqual({
      version: 1,
      critical_path_weight: 1_000_000,
      fanout_weight: 10_000,
      age_weight: 1_000,
      token_cost_weight: 1,
    })
  })

  it("rejects the retired wall-budget field", () => {
    expect(() => schedulerPolicyToKernel({
      version: 1,
      criticalPathWeight: 1,
      fanoutWeight: 1,
      ageWeight: 1,
      tokenCostWeight: 1,
      maxWallMs: 1234,
    } as any)).toThrow(/unknown scheduler policy field.*maxWallMs/)
  })
})
