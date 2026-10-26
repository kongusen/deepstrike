import { readFileSync } from "node:fs"
import type { CanonicalKernelInstance } from "../../src/kernel.js"
import { InMemoryKernelJournal } from "../../src/runtime/kernel-journal.js"
import { CanonicalRunnerRuntime } from "../../src/runtime/canonical-kernel-step.js"

/** Shared host-event mappings. Capturing commit checks a single terminal resolution (including
 * pending); kernel idempotency and persistence are deliberately outside this bridge test. */
interface ReceiptCase {
  id: string
  event: Record<string, unknown>
  expected: Record<string, unknown>
}

const fixture = JSON.parse(readFileSync(new URL(
  "../../../tests/fixtures/abi/memory_candidate_receipts.json", import.meta.url,
), "utf8")) as { binding_id: string; effect_id: string; cases: ReceiptCase[] }

async function resolve(event: Record<string, unknown>): Promise<Record<string, unknown>> {
  // commit is intercepted below, so this mapping test must never call the native kernel.
  const kernel = {} as CanonicalKernelInstance
  const runtime = new CanonicalRunnerRuntime(kernel, new InMemoryKernelJournal(), "op-candidate-bridge", {
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
