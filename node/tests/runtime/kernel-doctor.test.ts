import { jest } from "@jest/globals"
import type { CanonicalKernelInstance, CanonicalRestoreCost } from "../../src/kernel.js"
import { InMemoryKernelJournal } from "../../src/runtime/kernel-journal.js"
import { diagnoseKernelJournal } from "../../src/runtime/kernel-doctor.js"

const OPERATION_ID = "doctor-op-1"

function cost(records: number): CanonicalRestoreCost {
  return {
    recordsBeforeCheckpoint: "0",
    tailInputsReplayed: String(records),
    recordsAfterCheckpoint: "0",
    bytesRead: "0",
  }
}

// A kernel that only does the four things a diagnosis touches: restore + report pending/terminal.
function diagnosticKernel(restore: (records: number) => CanonicalRestoreCost): CanonicalKernelInstance {
  return {
    restore: (_checkpoint: Buffer | undefined, records: Buffer[]) => restore(records.length),
    lifecycle: () => "running",
    pendingEffectsJson: () =>
      JSON.stringify([{
        effect_id: "doctor-op-1:step:9:effect:0",
        causation_input_id: "in-9",
        effect: { kind: "query_memory" },
      }]),
    terminalJson: () => undefined,
  } as unknown as CanonicalKernelInstance
}

async function seededJournal(count: number): Promise<InMemoryKernelJournal> {
  const journal = new InMemoryKernelJournal()
  let head: string | undefined
  for (let i = 0; i < count; i += 1) {
    const receipt = await journal.compareAndAppend(OPERATION_ID, head, {
      step_seq: i,
      record_digest: `digest-${i}`,
      record_bytes: new Uint8Array(Buffer.from(`record:${i}`)),
    })
    head = receipt.record_digest
  }
  return journal
}

describe("diagnoseKernelJournal", () => {
  it("reports a journal that restores cleanly, with pending effects", async () => {
    const journal = await seededJournal(3)
    const diagnosis = await diagnoseKernelJournal(
      diagnosticKernel(records => cost(records)),
      journal,
      OPERATION_ID,
    )
    expect(diagnosis.restorable).toBe(true)
    expect(diagnosis.recordCount).toBe(3)
    expect(diagnosis.divergenceStep).toBeNull()
    expect(diagnosis.pendingEffects).toEqual([{ effect_id: "doctor-op-1:step:9:effect:0", kind: "query_memory" }])
  })

  it("names the diverging record when replay cannot reproduce the digest", async () => {
    const journal = await seededJournal(9)
    const diagnosis = await diagnoseKernelJournal(
      diagnosticKernel(() => {
        throw new Error("replaying the transition at step 7 produced record digest aabb against the durable ccdd")
      }),
      journal,
      OPERATION_ID,
    )
    expect(diagnosis.restorable).toBe(false)
    expect(diagnosis.divergenceStep).toBe(7)
    expect(diagnosis.divergenceReason).toContain("step 7")
    expect(diagnosis.pendingEffects).toEqual([])
  })
})
