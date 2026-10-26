import { getKernel } from "../../src/kernel.js"
import { InMemoryKernelJournal } from "../../src/runtime/kernel-journal.js"
import { CanonicalRunnerRuntime } from "../../src/runtime/canonical-kernel-step.js"

/** M2 bridge: how a host's `memory_candidate_result` becomes the kernel effect input. The commit
 *  is replaced so the test sees the resolution the bridge built, without driving a full run. */

async function resolve(event: Record<string, unknown>): Promise<Record<string, unknown>> {
  const { CanonicalKernel } = getKernel()
  const runtime = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-candidate-bridge", {
    maxContextTokens: 100_000,
  })
  const internals = runtime as unknown as {
    started: boolean
    commit: (input: Record<string, unknown>) => Promise<null>
  }
  internals.started = true
  let captured: Record<string, unknown> | undefined
  internals.commit = async input => {
    captured = input
    return null
  }
  await runtime.applyHostEvent({ effect_id: "effect-7", ...event })
  if (!captured) throw new Error("the bridge produced no resolution")
  return captured
}

describe("memory_candidate_result bridge (M2)", () => {
  it("pending resolves the effect with a candidate and no record receipt", async () => {
    const input = await resolve({ kind: "memory_candidate_result", status: "pending", candidate_id: "cand-1", receipt_id: "rcpt-1" })
    expect(input).toMatchObject({
      kind: "resolve_effect",
      effect_id: "effect-7",
      outcome: {
        status: "succeeded",
        result: {
          kind: "memory_persisted",
          candidate: { candidate_id: "cand-1", receipt_id: "rcpt-1" },
        },
      },
    })
    const result = (input.outcome as { result: Record<string, unknown> }).result
    expect(result).not.toHaveProperty("receipt")
  })

  it("accepted resolves like a persisted record, with a receipt and no candidate", async () => {
    const input = await resolve({
      kind: "memory_candidate_result", status: "accepted", record_ref: "rec-3", digest: "sha256:" + "0".repeat(64),
    })
    const result = (input.outcome as { result: Record<string, unknown> }).result
    expect(result).toMatchObject({ kind: "memory_persisted", receipt: { record_ref: "rec-3" } })
    expect(result).not.toHaveProperty("candidate")
  })

  it("rejected fails the effect non-retryably and names the reason", async () => {
    const input = await resolve({ kind: "memory_candidate_result", status: "rejected", reason_code: "source_unverifiable" })
    expect(input.outcome).toMatchObject({
      status: "failed",
      failure: { kind: "storage_unavailable", retryable: false },
    })
    expect((input.outcome as { failure: { message: string } }).failure.message).toContain("source_unverifiable")
  })

  it("a transport error fails the effect retryably, so the same candidate can be redelivered", async () => {
    const input = await resolve({ kind: "memory_candidate_result", error: "sink timeout" })
    expect(input.outcome).toMatchObject({ status: "failed", failure: { retryable: true } })
  })

  it("a pending answer that names no candidate is a non-retryable failure, not a success", async () => {
    const input = await resolve({ kind: "memory_candidate_result", status: "pending", receipt_id: "rcpt-1" })
    expect(input.outcome).toMatchObject({ status: "failed", failure: { retryable: false } })
  })
})
