import {
  kernelObservationToSessionEventAtBoundary,
  type KernelObservationSessionEventRequest,
} from "../src/runtime/kernel-event-log.js"

describe("kernel observation to session event boundary", () => {
  it("decodes representative kernel observations through the typed request", () => {
    const request: KernelObservationSessionEventRequest = {
      observation: {
        kind: "entropy_sample",
        turn: 7,
        score: 0.42,
        rho: 0.2,
        repeat_pressure: 0.1,
        failure_rate: 0.05,
        rollbacks_in_window: 1,
        window_turns: 4,
      },
      turn: 8,
    }

    expect(kernelObservationToSessionEventAtBoundary(request)).toEqual({
      kind: "entropy_sample",
      turn: 7,
      score: 0.42,
      rho: 0.2,
      repeat_pressure: 0.1,
      failure_rate: 0.05,
      rollbacks_in_window: 1,
      window_turns: 4,
    })
  })

  it("preserves explicit archive context for compressed observations", () => {
    const event = kernelObservationToSessionEventAtBoundary({
      observation: { kind: "compressed", summary: "compact", action: "auto_compact" },
      turn: 3,
      options: {
        nextArchiveStart: 10,
        latestSeq: 12,
        preservedRefs: ["ref-1"],
        compressionAction: () => "auto_compact",
      },
    })

    expect(event).toMatchObject({
      kind: "compressed",
      turn: 3,
      archived_seq_range: [10, 12],
      action: "auto_compact",
      preserved_refs: ["ref-1"],
    })
  })

  it("keeps non-persisted page-in requests out of the session log", () => {
    expect(
      kernelObservationToSessionEventAtBoundary({
        observation: { kind: "page_in_requested", entry_count: 2 },
        turn: 4,
      }),
    ).toBeNull()
  })
})
