import {
  DriverKernelJournal,
  InMemoryJournalDriver,
  InMemoryKernelJournal,
  JournalCasConflictError,
  JournalIntegrityError,
} from "../src/runtime/kernel-journal.js"
import type {
  CheckpointCandidate,
  JournalRecordInput,
  JournalStorageDriver,
  KernelJournal,
} from "../src/runtime/kernel-journal.js"

const OP = "op-journal"
const encoder = new TextEncoder()
const decoder = new TextDecoder()

function bytes(text: string): Uint8Array {
  return encoder.encode(text)
}

/** A record shaped like core's output: opaque bytes + the digest core assigned them. */
function record(stepSeq: number, digest: string, payload = `payload-${stepSeq}`): JournalRecordInput {
  return { step_seq: stepSeq, record_digest: digest, record_bytes: bytes(payload) }
}

function candidate(id: string, throughStepSeq: number): CheckpointCandidate {
  return {
    checkpoint_id: id,
    through_step_seq: throughStepSeq,
    state_digest: `state-${id}`,
    checkpoint_bytes: bytes(`checkpoint-${id}`),
  }
}

/** Append `count` linked records after genesis; returns every digest in chain order. */
async function seedChain(journal: KernelJournal, count: number, operationId = OP): Promise<string[]> {
  const digests = ["d0"]
  await journal.compareAndAppend(operationId, undefined, record(0, "d0"))
  for (let seq = 1; seq <= count; seq++) {
    await journal.compareAndAppend(operationId, digests[seq - 1], record(seq, `d${seq}`))
    digests.push(`d${seq}`)
  }
  return digests
}

describe.each([
  ["InMemoryKernelJournal", () => new InMemoryKernelJournal() as KernelJournal],
  ["DriverKernelJournal", () => new DriverKernelJournal(new InMemoryJournalDriver()) as KernelJournal],
] as const)("%s — KernelJournal contract", (_name, make) => {
  let journal: KernelJournal

  beforeEach(() => {
    journal = make()
  })

  it("starts an empty chain with a genesis append and advances the head", async () => {
    expect(await journal.head(OP)).toBeUndefined()

    const receipt = await journal.compareAndAppend(OP, undefined, record(0, "d0"))
    expect(receipt).toEqual({ step_seq: 0, record_digest: "d0" })
    expect(await journal.head(OP)).toEqual({ step_seq: 0, record_digest: "d0" })

    await journal.compareAndAppend(OP, "d0", record(1, "d1"))
    expect(await journal.head(OP)).toEqual({ step_seq: 1, record_digest: "d1" })
  })

  it("stores record bytes verbatim and links each record to its CAS precondition", async () => {
    await seedChain(journal, 2)
    const entries = await journal.readFrom(OP)

    expect(entries.map(entry => entry.step_seq)).toEqual([0, 1, 2])
    expect(entries.map(entry => entry.previous_record_digest)).toEqual([undefined, "d0", "d1"])
    expect(decoder.decode(entries[2].record_bytes)).toBe("payload-2")
  })

  it("rejects an append whose expected head is stale, and does not overwrite", async () => {
    await seedChain(journal, 1)

    await expect(journal.compareAndAppend(OP, "d0", record(1, "other"))).rejects.toBeInstanceOf(
      JournalCasConflictError,
    )
    // The winner is untouched: same head, same bytes, no fork.
    expect(await journal.head(OP)).toEqual({ step_seq: 1, record_digest: "d1" })
    expect(await journal.readFrom(OP)).toHaveLength(2)
  })

  it("rejects a second genesis on a non-empty chain", async () => {
    await journal.compareAndAppend(OP, undefined, record(0, "d0"))
    await expect(journal.compareAndAppend(OP, undefined, record(0, "other"))).rejects.toBeInstanceOf(
      JournalCasConflictError,
    )
  })

  it("separates a CAS conflict from an integrity violation", async () => {
    await seedChain(journal, 1)

    // Head matches, but the record claims a position that does not follow it.
    await expect(journal.compareAndAppend(OP, "d1", record(5, "d5"))).rejects.toBeInstanceOf(
      JournalIntegrityError,
    )
    // A genesis record on a chain that already has one is a conflict, not an integrity fault.
    await expect(journal.compareAndAppend(OP, undefined, record(0, "d0"))).rejects.toBeInstanceOf(
      JournalCasConflictError,
    )
  })

  it("reads by step cursor and by digest cursor", async () => {
    const digests = await seedChain(journal, 3)

    expect((await journal.readFrom(OP, 2)).map(entry => entry.step_seq)).toEqual([2, 3])
    expect((await journal.recordsAfter(OP, digests[1])).map(entry => entry.step_seq)).toEqual([2, 3])
    expect((await journal.recordsAfter(OP)).map(entry => entry.step_seq)).toEqual([0, 1, 2, 3])
    await expect(journal.recordsAfter(OP, "not-a-record")).rejects.toBeInstanceOf(JournalIntegrityError)
  })

  it("keeps operations isolated", async () => {
    await seedChain(journal, 1, "op-a")
    await seedChain(journal, 2, "op-b")

    expect(await journal.head("op-a")).toEqual({ step_seq: 1, record_digest: "d1" })
    expect(await journal.head("op-b")).toEqual({ step_seq: 2, record_digest: "d2" })
  })

  it("installs a checkpoint whose covered head is no longer the current head (§22.14)", async () => {
    const digests = await seedChain(journal, 3)

    // Candidate covers step 1; steps 2 and 3 were appended after it was taken.
    const installed = await journal.compareAndInstallCheckpoint(OP, undefined, digests[1], candidate("ck-1", 1))

    expect(installed.ordinal).toBe(0)
    expect(installed.covered_head).toBe("d1")
    expect(installed.acknowledged).toBe(false)
    expect(await journal.head(OP)).toEqual({ step_seq: 3, record_digest: "d3" })
    expect((await journal.latestCheckpoint(OP))?.checkpoint_id).toBe("ck-1")
    // The tail survives the install.
    expect((await journal.readFrom(OP)).map(entry => entry.step_seq)).toEqual([0, 1, 2, 3])
  })

  it("rejects a checkpoint whose covered head disagrees with its through_step_seq", async () => {
    const digests = await seedChain(journal, 2)

    await expect(
      journal.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-1", 1)),
    ).rejects.toBeInstanceOf(JournalIntegrityError)
    await expect(
      journal.compareAndInstallCheckpoint(OP, undefined, "d9", candidate("ck-1", 9)),
    ).rejects.toBeInstanceOf(JournalIntegrityError)
    expect(await journal.latestCheckpoint(OP)).toBeUndefined()
  })

  it("advances the checkpoint pointer monotonically under CAS", async () => {
    const digests = await seedChain(journal, 3)
    await journal.compareAndInstallCheckpoint(OP, undefined, digests[1], candidate("ck-1", 1))

    // Installing again without naming the predecessor is a conflict.
    await expect(
      journal.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-2", 2)),
    ).rejects.toBeInstanceOf(JournalCasConflictError)
    // Naming a stale predecessor is a conflict.
    await expect(
      journal.compareAndInstallCheckpoint(OP, "ck-0", digests[2], candidate("ck-2", 2)),
    ).rejects.toBeInstanceOf(JournalCasConflictError)
    // Naming the current predecessor but moving backwards is an integrity fault.
    await expect(
      journal.compareAndInstallCheckpoint(OP, "ck-1", digests[0], candidate("ck-2", 0)),
    ).rejects.toBeInstanceOf(JournalIntegrityError)

    const second = await journal.compareAndInstallCheckpoint(OP, "ck-1", digests[2], candidate("ck-2", 2))
    expect(second.ordinal).toBe(1)
    expect(second.previous_checkpoint_id).toBe("ck-1")
    expect((await journal.latestCheckpoint(OP))?.checkpoint_id).toBe("ck-2")
  })

  it("gates prefix reclamation on the acknowledgement boundary", async () => {
    const digests = await seedChain(journal, 3)
    await journal.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-1", 2))

    // Installed but unacknowledged: nothing is reclaimed.
    expect(await journal.pruneAckedPrefix(OP)).toEqual({ pruned_through_step_seq: -1, pruned_count: 0 })
    expect(await journal.readFrom(OP)).toHaveLength(4)

    const acked = await journal.ackCheckpoint(OP, "ck-1")
    expect(acked.acknowledged).toBe(true)
    expect((await journal.latestCheckpoint(OP))?.acknowledged).toBe(true)

    expect(await journal.pruneAckedPrefix(OP)).toEqual({ pruned_through_step_seq: 2, pruned_count: 3 })
    expect((await journal.readFrom(OP)).map(entry => entry.step_seq)).toEqual([3])
    // The pruned boundary is retained as an anchor, so head and the digest cursor still resolve.
    expect(await journal.head(OP)).toEqual({ step_seq: 3, record_digest: "d3" })
    expect((await journal.recordsAfter(OP, digests[2])).map(entry => entry.step_seq)).toEqual([3])
    // And the chain keeps growing from the surviving head.
    await journal.compareAndAppend(OP, "d3", record(4, "d4"))
    expect(await journal.head(OP)).toEqual({ step_seq: 4, record_digest: "d4" })
  })

  it("refuses to acknowledge an uninstalled checkpoint", async () => {
    await seedChain(journal, 1)
    await expect(journal.ackCheckpoint(OP, "ck-missing")).rejects.toBeInstanceOf(JournalIntegrityError)
  })
})

describe("DriverKernelJournal — the driver's atomic claim decides every race", () => {
  let driver: JournalStorageDriver

  beforeEach(() => {
    driver = new InMemoryJournalDriver()
  })

  it("lets exactly one of two concurrent writers append at the same chain position", async () => {
    // Two journal instances over ONE driver = two writers with no shared in-instance lock. Only the
    // driver's atomic claim can decide this race; `Promise.allSettled` runs both interleaved.
    const a = new DriverKernelJournal(driver)
    const b = new DriverKernelJournal(driver)
    await a.compareAndAppend(OP, undefined, record(0, "d0"))

    const results = await Promise.allSettled([
      a.compareAndAppend(OP, "d0", record(1, "from-a", "a")),
      b.compareAndAppend(OP, "d0", record(1, "from-b", "b")),
    ])

    expect(results.filter(result => result.status === "fulfilled")).toHaveLength(1)
    const rejected = results.find(result => result.status === "rejected") as PromiseRejectedResult
    expect(rejected.reason).toBeInstanceOf(JournalCasConflictError)

    // The journal did not fork: one record at step 1, and every reader agrees on it.
    const entries = await new DriverKernelJournal(driver).readFrom(OP)
    expect(entries.map(entry => entry.step_seq)).toEqual([0, 1])
    const winner = (results.find(r => r.status === "fulfilled") as PromiseFulfilledResult<{ record_digest: string }>).value
    expect(entries[1].record_digest).toBe(winner.record_digest)
    expect((await a.head(OP))?.record_digest).toBe(winner.record_digest)
    expect((await b.head(OP))?.record_digest).toBe(winner.record_digest)
  })

  it("survives a wide concurrent append storm with a single winner per position", async () => {
    const writers = Array.from({ length: 8 }, () => new DriverKernelJournal(driver))
    await writers[0].compareAndAppend(OP, undefined, record(0, "d0"))

    const results = await Promise.allSettled(
      writers.map((writer, i) => writer.compareAndAppend(OP, "d0", record(1, `d1-${i}`, `w${i}`))),
    )

    expect(results.filter(result => result.status === "fulfilled")).toHaveLength(1)
    for (const result of results.filter(r => r.status === "rejected") as PromiseRejectedResult[]) {
      expect(result.reason).toBeInstanceOf(JournalCasConflictError)
    }
    expect(await new DriverKernelJournal(driver).readFrom(OP)).toHaveLength(2)
  })

  it("lets exactly one of two concurrent writers install a checkpoint", async () => {
    const a = new DriverKernelJournal(driver)
    const b = new DriverKernelJournal(driver)
    const digests = await seedChain(a, 2)

    const results = await Promise.allSettled([
      a.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-a", 2)),
      b.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-b", 2)),
    ])

    expect(results.filter(result => result.status === "fulfilled")).toHaveLength(1)
    const rejected = results.find(result => result.status === "rejected") as PromiseRejectedResult
    expect(rejected.reason).toBeInstanceOf(JournalCasConflictError)

    const installed = await new DriverKernelJournal(driver).latestCheckpoint(OP)
    expect(["ck-a", "ck-b"]).toContain(installed?.checkpoint_id)
    expect(installed?.ordinal).toBe(0)
    // Exactly one ordinal was claimed — the loser did not land on a name of its own.
    expect(await driver.list(`kernel-journal/${OP}/checkpoints/`)).toEqual([
      `kernel-journal/${OP}/checkpoints/000000000000.ckpt`,
    ])
  })

  it("lets the atomic claim — not the pre-check — decide the append", async () => {
    // The pre-claim head read cannot be the fence: another writer may commit between it and the
    // publish. Simulate exactly that interleaving by holding the pre-check's view at a stale head
    // while the position it computes is already taken. If acceptance were decided in userspace this
    // append would succeed and fork the chain; it must lose to the storage layer instead.
    class StalePreCheck extends DriverKernelJournal {
      override async head(): Promise<{ step_seq: number; record_digest: string }> {
        return { step_seq: 0, record_digest: "d0" }
      }
    }
    const committed = new DriverKernelJournal(driver)
    await committed.compareAndAppend(OP, undefined, record(0, "d0"))
    await committed.compareAndAppend(OP, "d0", record(1, "winner", "winner"))

    await expect(
      new StalePreCheck(driver).compareAndAppend(OP, "d0", record(1, "loser", "loser")),
    ).rejects.toBeInstanceOf(JournalCasConflictError)

    const entries = await committed.readFrom(OP)
    expect(entries.map(entry => entry.record_digest)).toEqual(["d0", "winner"])
    expect(decoder.decode(entries[1].record_bytes)).toBe("winner")
  })

  it("lets the atomic claim decide the checkpoint install too", async () => {
    class StalePreCheck extends DriverKernelJournal {
      override async latestCheckpoint(): Promise<undefined> {
        return undefined
      }
    }
    const committed = new DriverKernelJournal(driver)
    const digests = await seedChain(committed, 2)
    await committed.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-winner", 2))

    await expect(
      new StalePreCheck(driver).compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-loser", 2)),
    ).rejects.toBeInstanceOf(JournalCasConflictError)

    expect((await committed.latestCheckpoint(OP))?.checkpoint_id).toBe("ck-winner")
  })

  it("reopens and verifies the chain, ignoring residue", async () => {
    const journal = new DriverKernelJournal(driver)
    const digests = await seedChain(journal, 3)

    // Residue a crashed or unrelated writer can leave in the same key space. Nothing that fails the
    // naming rule may be mistaken for a committed record.
    await driver.put(`kernel-journal/${OP}/records/000000000004.rec.partial`, bytes('{"step_seq":4'))
    await driver.put(`kernel-journal/${OP}/records/notes.txt`, bytes("scratch"))
    await driver.put(`kernel-journal/${OP}/tmp/half-written`, bytes('{"step_seq":4,"record_dig'))

    const reopened = new DriverKernelJournal(driver)
    const entries = await reopened.readFrom(OP)
    expect(entries.map(entry => entry.step_seq)).toEqual([0, 1, 2, 3])
    expect(entries.map(entry => entry.record_digest)).toEqual(digests)
    expect(await reopened.head(OP)).toEqual({ step_seq: 3, record_digest: "d3" })
    // The chain still accepts its next record, so residue did not poison the CAS position either.
    await reopened.compareAndAppend(OP, "d3", record(4, "d4"))
    expect(await reopened.head(OP)).toEqual({ step_seq: 4, record_digest: "d4" })
  })

  it("reopens installed and acknowledged checkpoints", async () => {
    const journal = new DriverKernelJournal(driver)
    const digests = await seedChain(journal, 2)
    await journal.compareAndInstallCheckpoint(OP, undefined, digests[1], candidate("ck-1", 1))
    await journal.ackCheckpoint(OP, "ck-1")

    const reopened = new DriverKernelJournal(driver)
    const latest = await reopened.latestCheckpoint(OP)
    expect(latest?.checkpoint_id).toBe("ck-1")
    expect(latest?.acknowledged).toBe(true)
    expect(latest?.covered_head).toBe("d1")
    expect(latest?.through_step_seq).toBe(1)
    expect(decoder.decode(latest!.checkpoint_bytes)).toBe("checkpoint-ck-1")
  })

  it("raises an integrity fault when a committed record contradicts its own name", async () => {
    const journal = new DriverKernelJournal(driver)
    await seedChain(journal, 1)
    await driver.put(
      `kernel-journal/${OP}/records/000000000002.rec`,
      bytes(JSON.stringify({ step_seq: 7 })),
    )

    await expect(new DriverKernelJournal(driver).readFrom(OP)).rejects.toBeInstanceOf(JournalIntegrityError)
  })

  it("keys records by CAS-precondition identity only, never by the racer's own digest", async () => {
    // Task 8b correction (a): the key is the collision domain. Two racers must compute the SAME key
    // from the predecessor they raced against, which is exactly what makes one of them lose.
    const journal = new DriverKernelJournal(driver)
    await seedChain(journal, 1)

    const keys = await driver.list(`kernel-journal/${OP}/records/`)
    expect(keys.sort()).toEqual([
      `kernel-journal/${OP}/records/000000000000.rec`,
      `kernel-journal/${OP}/records/000000000001.rec`,
    ])
    for (const key of keys) expect(key).not.toContain("d1")
  })
})

describe("KernelJournal outbound envelopes", () => {
  it("stages and clears outbound envelopes (InMemory + Driver)", async () => {
    const memory = new InMemoryKernelJournal()
    await memory.stageOutboundEnvelope("op-out", "{\"kind\":\"start_operation\"}")
    expect(await memory.readOutboundEnvelope("op-out")).toBe("{\"kind\":\"start_operation\"}")
    await memory.clearOutboundEnvelope("op-out")
    expect(await memory.readOutboundEnvelope("op-out")).toBeUndefined()

    const driver = new DriverKernelJournal(new InMemoryJournalDriver())
    await driver.stageOutboundEnvelope("op-out", "{\"kind\":\"resolve_effect\"}")
    expect(await driver.readOutboundEnvelope("op-out")).toBe("{\"kind\":\"resolve_effect\"}")
    await driver.clearOutboundEnvelope("op-out")
    expect(await driver.readOutboundEnvelope("op-out")).toBeUndefined()
  })

})
