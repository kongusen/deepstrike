import { createEvolutionRuntimeAdapter, EvolutionRuntime, EvolutionStore } from "../src/runtime/evolution.js"

const bundle = {
  artifacts: [], artifact_sets: [], proposals: [], evaluations: [], facts: [], decisions: [],
  activations: [{ digest: "sha256:activation", operation_id: "op-1", artifact_set: "sha256:set", promotion_decision: "sha256:decision" }],
}

describe("Evolution Runtime mirror", () => {
  test("delegates validation to the single core adapter", () => {
    const requests: string[] = []
    const runtime = new EvolutionRuntime(createEvolutionRuntimeAdapter((request) => {
      requests.push(request)
      return JSON.stringify({ schema: "evolution-report/v1", verdict: "pass", violations: [] })
    }))
    const report = runtime.validate(bundle)
    expect(report.verdict).toBe("pass")
    expect(JSON.parse(requests[0]).artifact_sets).toEqual([])
  })

  test("validates a host-owned store before activation", async () => {
    const runtime = new EvolutionRuntime(createEvolutionRuntimeAdapter(() =>
      JSON.stringify({ schema: "evolution-report/v1", verdict: "pass", violations: [] })))
    const store: EvolutionStore = { loadBundle: () => bundle }
    await expect(runtime.validateStore(store)).resolves.toMatchObject({ verdict: "pass" })
    expect(runtime.activate(bundle, "op-1").artifact_set).toBe("sha256:set")
  })

  test("fails closed when the canonical report rejects activation", () => {
    const runtime = new EvolutionRuntime(createEvolutionRuntimeAdapter(() =>
      JSON.stringify({ schema: "evolution-report/v1", verdict: "fail", violations: [{ code: "E7", detail: "gate" }] })))
    expect(() => runtime.activate(bundle, "op-1")).toThrow("E7")
  })
})
