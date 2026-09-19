import { createEvolutionRuntimeAdapter, EvolutionRuntime } from "../src/runtime/evolution.js"

describe("Evolution Runtime mirror", () => {
  test("delegates validation to the single core adapter", () => {
    const requests: string[] = []
    const runtime = new EvolutionRuntime(createEvolutionRuntimeAdapter((request) => {
      requests.push(request)
      return JSON.stringify({ schema: "evolution-report/v1", verdict: "pass", violations: [] })
    }))
    const report = runtime.validate({
      artifacts: [], artifact_sets: [], proposals: [], evaluations: [], facts: [], decisions: [], activations: [],
    })
    expect(report.verdict).toBe("pass")
    expect(JSON.parse(requests[0]).artifact_sets).toEqual([])
  })
})
