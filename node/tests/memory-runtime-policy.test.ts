import { resolveMemoryRuntimePolicy, type MemoryRuntimePolicy } from "../src/memory/runtime-policy.js"

const recallOnly: MemoryRuntimePolicy = {
  retrieval: "host",
  modelWrites: "disabled",
}

describe("memory runtime policy (M1)", () => {
  it("leaves every path enabled when no policy is supplied", () => {
    expect(resolveMemoryRuntimePolicy(undefined)).toBeUndefined()
  })

  it("projects a recall-only policy onto the runner gates", () => {
    expect(resolveMemoryRuntimePolicy(recallOnly)).toEqual({
      retrievalEnabled: true,
      modelWritesEnabled: false,
      modelWritesToCandidateSink: false,
    })
  })

  it("turns retrieval off independently of the write paths", () => {
    expect(resolveMemoryRuntimePolicy({ ...recallOnly, retrieval: "disabled" })).toMatchObject({
      retrievalEnabled: false,
      modelWritesEnabled: false,
    })
  })

  it("candidate_sink keeps a write path on and routes it to the sink (M2)", () => {
    expect(resolveMemoryRuntimePolicy({ ...recallOnly, modelWrites: "candidate_sink" })).toMatchObject({
      modelWritesEnabled: true,
      modelWritesToCandidateSink: true,
    })
  })

  it("rejects unknown field names", () => {
    expect(() => resolveMemoryRuntimePolicy({ ...recallOnly, storage: "x" } as never)).toThrow(
      "unknown memory runtime policy field(s): storage",
    )
  })

  it("rejects values outside the allowed set for each field", () => {
    expect(() => resolveMemoryRuntimePolicy({ ...recallOnly, retrieval: "candidate_sink" } as never)).toThrow(
      "memory runtime policy retrieval must be one of disabled | host",
    )
    expect(() => resolveMemoryRuntimePolicy({ ...recallOnly, modelWrites: "host" } as never)).toThrow(
      "memory runtime policy modelWrites must be one of disabled | candidate_sink",
    )
  })

  it("rejects a missing field instead of defaulting it", () => {
    const partial = { retrieval: "host" } as never
    expect(() => resolveMemoryRuntimePolicy(partial)).toThrow("memory runtime policy modelWrites must be one of")
  })
})
