import { readFileSync } from "node:fs"
import { join } from "node:path"

import {
  canonicalActionFromPlannedStep,
  canonicalUnsupportedEffectResolution,
} from "../src/runtime/canonical-kernel-step.js"

describe("unknown canonical effect contract", () => {
  it("preserves correlation and returns the shared ProtocolError resolution", () => {
    const fixture = JSON.parse(readFileSync(
      join(process.cwd(), "../tests/fixtures/abi/unknown_effect_protocol_error.json"),
      "utf8",
    )) as {
      planned_step: Parameters<typeof canonicalActionFromPlannedStep>[0]
      expected_action: Record<string, unknown>
      expected_resolution: Record<string, unknown>
    }

    const action = canonicalActionFromPlannedStep(fixture.planned_step)
    expect(action).toMatchObject({
      kind: fixture.expected_action.kind,
      effectId: fixture.expected_action.effect_id,
      effectKind: fixture.expected_action.effect_kind,
    })
    if (action?.kind !== "unsupported_effect") throw new Error("expected unsupported effect action")
    expect(canonicalUnsupportedEffectResolution(action.effectId, action.effectKind))
      .toEqual(fixture.expected_resolution)
  })
})
