import { readFileSync } from "node:fs"
import { join } from "node:path"
import { canonicalActionFromProjectionJson } from "../src/runtime/canonical-kernel-step.js"

test("WASM adapter consumes the shared CurrentProjection selector", () => {
  const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/abi/current_projection_multi_effect.json"), "utf8"))
  const expected = fixture.expected
  const action = canonicalActionFromProjectionJson(JSON.stringify({
    state: "action",
    action: {
      kind: expected.action_kind,
      effect_id: expected.effect_id,
      causation_input_id: expected.causation_input_id,
      payload: { requested_k: expected.payload_requested_k, query: { text: expected.payload_query_text } },
    },
  }))
  expect(action?.kind).toBe("query_memory")
  expect(action?.effectId).toBe(expected.effect_id)
})
