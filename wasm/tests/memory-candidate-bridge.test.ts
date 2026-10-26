import { readFileSync } from "node:fs"
import { CanonicalKernel } from "@deepstrike/wasm-kernel"
import { CanonicalRunnerRuntime, canonicalActionFromProjectionJson } from "../src/runtime/canonical-kernel-step.js"
import { InMemoryKernelJournal } from "../src/runtime/kernel-journal.js"

/** Shared host-event mappings. Capturing commit checks a single terminal resolution (including
 * pending); kernel idempotency and persistence are deliberately outside this bridge test. */
interface ReceiptCase {
  id: string
  event: Record<string, unknown>
  expected: Record<string, unknown>
}

const fixture = JSON.parse(readFileSync(new URL(
  "../../tests/fixtures/abi/memory_candidate_receipts.json", import.meta.url,
), "utf8")) as { binding_id: string; effect_id: string; cases: ReceiptCase[] }

async function resolve(event: Record<string, unknown>): Promise<Record<string, unknown>> {
  const runtime = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-candidate-bridge", {
    maxContextTokens: 100_000,
    memoryBindingId: fixture.binding_id,
  })
  const internals = runtime as unknown as {
    started: boolean
    commit: (input: Record<string, unknown>) => Promise<null>
  }
  internals.started = true
  const captured: Record<string, unknown>[] = []
  internals.commit = async input => {
    captured.push(input)
    return null
  }
  await runtime.applyHostEvent({ ...event, effect_id: fixture.effect_id })
  expect(captured).toHaveLength(1)
  return captured[0]
}

describe("memory_candidate_result shared receipt fixtures", () => {
  it.each(fixture.cases)("$id", async testCase => {
    const input = await resolve(testCase.event)
    const expected = testCase.expected
    // Exact equality also forbids a result containing both candidate and receipt arms.
    expect(input).toStrictEqual({
      kind: "resolve_effect",
      effect_id: fixture.effect_id,
      outcome: expected,
    })
  })
})

it("preserves semantic archive metadata from committed observations", () => {
  const action = canonicalActionFromProjectionJson(JSON.stringify({
    state: "action", action: { kind: "archive_page_out", effect_id: "archive:7",
      payload: { payload: { content: '[]' } } },
  }), [{ kind: "compressed", action: "auto_compact", summary: "old" }])
  expect(action).toMatchObject({ kind: "archive_page_out", action: "auto_compact", tier: "semantic", summary: "old" })
})

it.each([[false, false], [true, false], [false, true], [true, true]])(
  "projects independent memory read=%s and write=%s capabilities", async (read, write) => {
    const runtime = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-policy", { maxContextTokens: 10000 })
    await runtime.applyHostEvent({ kind: "set_memory_enabled", enabled: true, read, write })
    expect((runtime as unknown as { config: { memory_access: unknown } }).config.memory_access)
      .toMatchObject({ capabilities: { read, write } })
  },
)
