import { readFileSync } from "node:fs"
import { join } from "node:path"
import { canonicalActionFromProjectionJson } from "../../src/runtime/canonical-kernel-step.js"
import { archivePresentationFromObservations } from "../../src/runtime/kernel-step.js"
import { stableSemanticArchiveName } from "../../src/runtime/runner.js"

test("core CurrentProjection selector matches shared multi-effect fixture", () => {
  const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/abi/current_projection_multi_effect.json"), "utf8"))
  const expected = fixture.expected
  const projection = {
    state: expected.state,
    action: {
      kind: expected.action_kind,
      effect_id: expected.effect_id,
      causation_input_id: expected.causation_input_id,
      payload: {
        requested_k: expected.payload_requested_k,
        query: { text: expected.payload_query_text },
      },
    },
  }
  const action = canonicalActionFromProjectionJson(JSON.stringify(projection))
  expect(action?.kind).toBe("query_memory")
  expect(action?.effectId).toBe(expected.effect_id)
  expect(action && "requestedK" in action ? action.requestedK : undefined).toBe(expected.payload_requested_k)
})

test("archive presentation facts come from observations, not projection action", () => {
  const projection = canonicalActionFromProjectionJson(JSON.stringify({
    state: "action",
    action: {
      kind: "archive_page_out",
      effect_id: "op:effect:archive",
      payload: {
        handle_id: "handle:1",
        payload: { content: "[]", digest: "sha256:x", original_size: "2" },
      },
    },
  }))
  expect(projection?.kind).toBe("archive_page_out")
  expect(projection && "tier" in projection ? projection.tier : undefined).toBeUndefined()
  expect(archivePresentationFromObservations([
    { kind: "compressed", action: "auto_compact", summary: "semantic summary" },
  ])).toEqual({ action: "auto_compact", summary: "semantic summary", tier: "semantic" })
})

test("semantic archive idempotency key is stable across retries", () => {
  expect(stableSemanticArchiveName("op/a:effect:1")).toBe("page-out-op_a:effect:1")
  expect(stableSemanticArchiveName("op/a:effect:1")).toBe(stableSemanticArchiveName("op/a:effect:1"))
})
