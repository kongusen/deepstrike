import { readFileSync } from "node:fs"
import { join } from "node:path"

import {
  canonicalActionFromProjectionJson,
  canonicalUnsupportedEffectResolution,
} from "../src/runtime/canonical-kernel-step.js"

describe("unknown canonical effect contract", () => {
  it("preserves correlation and returns the shared ProtocolError resolution", () => {
    const fixture = JSON.parse(readFileSync(
      join(process.cwd(), "../tests/fixtures/abi/unknown_effect_protocol_error.json"),
      "utf8",
    )) as {
      planned_step: Record<string, unknown>
      expected_action: Record<string, unknown>
      expected_resolution: Record<string, unknown>
    }

    const action = canonicalActionFromProjectionJson(JSON.stringify({
      state: "action",
      action: {
        kind: fixture.expected_action.effect_kind,
        effect_id: fixture.expected_action.effect_id,
        causation_input_id: "in-unknown", payload: {},
      },
    }))
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
