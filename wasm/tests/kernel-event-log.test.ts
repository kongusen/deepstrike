import { kernelObservationToSessionEvent } from "../src/runtime/kernel-event-log.js"

describe("kernel observation audit projection", () => {
  it("preserves an unknown observation as an opaque durable envelope", () => {
    expect(kernelObservationToSessionEvent({
      kind: "future_observation",
      turn: 4,
      detail: "must survive",
    } as never, 4)).toEqual({
      kind: "kernel_observation",
      turn: 4,
      observation_kind: "future_observation",
      raw: {
        kind: "future_observation",
        turn: 4,
        detail: "must survive",
      },
    })
  })
})
