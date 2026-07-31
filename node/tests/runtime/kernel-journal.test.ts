import { mkdtemp, mkdir, readdir, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import {
  FileKernelJournal,
  InMemoryKernelJournal,
  JournalCasConflictError,
  JournalIntegrityError,
  MAX_CHAIN_POSITION,
} from "../../src/runtime/kernel-journal.js"
import type { CheckpointCandidate, JournalRecordInput, KernelJournal } from "../../src/runtime/kernel-journal.js"
import { InMemorySessionLog, FileSessionLog, journalOperationKey } from "../../src/runtime/session-log.js"
import {
  createKernelOperationGenesis,
  createKernelTransaction,
} from "../../src/runtime/kernel-transaction-log.js"

const OP = "op-journal"

function operationDir(root: string, operationId = OP): string {
  return join(root, `op-${Buffer.from(operationId, "utf8").toString("base64url")}`)
}

function bytes(text: string): Uint8Array {
  return new Uint8Array(Buffer.from(text, "utf8"))
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
  ["InMemoryKernelJournal", async () => ({ journal: new InMemoryKernelJournal() as KernelJournal, cleanup: async () => {} })],
  [
    "FileKernelJournal",
    async () => {
      const dir = await mkdtemp(join(tmpdir(), "ds-journal-"))
      return { journal: new FileKernelJournal(dir) as KernelJournal, cleanup: () => rm(dir, { recursive: true, force: true }) }
    },
  ],
] as const)("%s — KernelJournal contract", (_name, make) => {
  let journal: KernelJournal
  let cleanup: () => Promise<unknown>

  beforeEach(async () => {
    ;({ journal, cleanup } = await make())
  })

  afterEach(async () => {
    await cleanup()
  })

  it("stages an outbound envelope until clear, surviving FileKernelJournal remount", async () => {
    expect(await journal.readOutboundEnvelope(OP)).toBeUndefined()
    const envelope = JSON.stringify({
      abi_version: 3,
      operation_id: OP,
      input_id: "input-stage-1",
      observed_at_ms: "1753747200099",
      input: { kind: "configure_operation", config: {} },
    })
    await journal.stageOutboundEnvelope(OP, envelope)
    expect(await journal.readOutboundEnvelope(OP)).toBe(envelope)

    await journal.stageOutboundEnvelope(OP, `${envelope}+v2`)
    expect(await journal.readOutboundEnvelope(OP)).toBe(`${envelope}+v2`)

    await journal.clearOutboundEnvelope(OP)
    expect(await journal.readOutboundEnvelope(OP)).toBeUndefined()
    await journal.clearOutboundEnvelope(OP) // idempotent
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
    expect(Buffer.from(entries[2].record_bytes).toString("utf8")).toBe("payload-2")
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

  it("rejects chain positions at or above the cross-host namespace bound", async () => {
    await expect(
      journal.compareAndAppend(OP, undefined, record(MAX_CHAIN_POSITION, "too-large")),
    ).rejects.toBeInstanceOf(JournalIntegrityError)
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

describe("FileKernelJournal — cross-process atomicity", () => {
  let dir: string

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "ds-journal-cas-"))
  })

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true })
  })

  it("lets exactly one of two concurrent writers append at the same chain position", async () => {
    // Two independent instances = two writers with no shared in-process lock. Only the storage
    // layer's atomic claim can decide this race.
    const a = new FileKernelJournal(dir)
    const b = new FileKernelJournal(dir)
    await a.compareAndAppend(OP, undefined, record(0, "d0"))

    const results = await Promise.allSettled([
      a.compareAndAppend(OP, "d0", record(1, "from-a", "a")),
      b.compareAndAppend(OP, "d0", record(1, "from-b", "b")),
    ])

    expect(results.filter(result => result.status === "fulfilled")).toHaveLength(1)
    const rejected = results.find(result => result.status === "rejected") as PromiseRejectedResult
    expect(rejected.reason).toBeInstanceOf(JournalCasConflictError)

    // The journal did not fork: one record at step 1, and every reader agrees on it.
    const entries = await new FileKernelJournal(dir).readFrom(OP)
    expect(entries.map(entry => entry.step_seq)).toEqual([0, 1])
    const winner = (results.find(r => r.status === "fulfilled") as PromiseFulfilledResult<{ record_digest: string }>).value
    expect(entries[1].record_digest).toBe(winner.record_digest)
    expect((await a.head(OP))?.record_digest).toBe(winner.record_digest)
    expect((await b.head(OP))?.record_digest).toBe(winner.record_digest)
  })

  it("encodes hostile operation ids as one child segment", async () => {
    const operationId = "../escape/../../records"
    const journal = new FileKernelJournal(dir)
    await journal.compareAndAppend(operationId, undefined, record(0, "safe"))

    expect(await journal.head(operationId)).toEqual({ step_seq: 0, record_digest: "safe" })
    expect(await readdir(dir)).toEqual([
      `op-${Buffer.from(operationId, "utf8").toString("base64url")}`,
    ])
  })

  it("survives a wide concurrent append storm with a single winner per position", async () => {
    const writers = Array.from({ length: 8 }, () => new FileKernelJournal(dir))
    await writers[0].compareAndAppend(OP, undefined, record(0, "d0"))

    const results = await Promise.allSettled(
      writers.map((writer, i) => writer.compareAndAppend(OP, "d0", record(1, `d1-${i}`, `w${i}`))),
    )

    expect(results.filter(result => result.status === "fulfilled")).toHaveLength(1)
    for (const result of results.filter(r => r.status === "rejected") as PromiseRejectedResult[]) {
      expect(result.reason).toBeInstanceOf(JournalCasConflictError)
    }
    expect(await new FileKernelJournal(dir).readFrom(OP)).toHaveLength(2)
  })

  it("lets exactly one of two concurrent writers install a checkpoint", async () => {
    const a = new FileKernelJournal(dir)
    const b = new FileKernelJournal(dir)
    const digests = await seedChain(a, 2)

    const results = await Promise.allSettled([
      a.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-a", 2)),
      b.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-b", 2)),
    ])

    expect(results.filter(result => result.status === "fulfilled")).toHaveLength(1)
    const rejected = results.find(result => result.status === "rejected") as PromiseRejectedResult
    expect(rejected.reason).toBeInstanceOf(JournalCasConflictError)

    const installed = await new FileKernelJournal(dir).latestCheckpoint(OP)
    expect(["ck-a", "ck-b"]).toContain(installed?.checkpoint_id)
    expect(installed?.ordinal).toBe(0)
    expect(await readdir(join(operationDir(dir), "checkpoints"))).toEqual(["000000000000.ckpt"])
  })

  it("lets the atomic claim — not the pre-check — decide the append", async () => {
    // The pre-`link` head read cannot be the fence: another process may commit between it and the
    // publish. Simulate exactly that interleaving by holding the pre-check's view at a stale head
    // while the position it computes is already taken. If acceptance were decided in userspace this
    // append would succeed and fork the chain; it must lose to the storage layer instead.
    class StalePreCheck extends FileKernelJournal {
      override async head(): Promise<{ step_seq: number; record_digest: string }> {
        return { step_seq: 0, record_digest: "d0" }
      }
    }
    const committed = new FileKernelJournal(dir)
    await committed.compareAndAppend(OP, undefined, record(0, "d0"))
    await committed.compareAndAppend(OP, "d0", record(1, "winner", "winner"))

    await expect(
      new StalePreCheck(dir).compareAndAppend(OP, "d0", record(1, "loser", "loser")),
    ).rejects.toBeInstanceOf(JournalCasConflictError)

    const entries = await committed.readFrom(OP)
    expect(entries.map(entry => entry.record_digest)).toEqual(["d0", "winner"])
    expect(Buffer.from(entries[1].record_bytes).toString("utf8")).toBe("winner")
  })

  it("lets the atomic claim decide the checkpoint install too", async () => {
    class StalePreCheck extends FileKernelJournal {
      override async latestCheckpoint(): Promise<undefined> {
        return undefined
      }
    }
    const committed = new FileKernelJournal(dir)
    const digests = await seedChain(committed, 2)
    await committed.compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-winner", 2))

    await expect(
      new StalePreCheck(dir).compareAndInstallCheckpoint(OP, undefined, digests[2], candidate("ck-loser", 2)),
    ).rejects.toBeInstanceOf(JournalCasConflictError)

    expect((await committed.latestCheckpoint(OP))?.checkpoint_id).toBe("ck-winner")
  })

  it("reopens and verifies the chain, ignoring crash residue", async () => {
    const journal = new FileKernelJournal(dir)
    const digests = await seedChain(journal, 3)

    // Residue a crash can actually leave: a staged-but-unlinked temp file, plus anything that does
    // not match the record naming rule. Neither may be mistaken for a committed record.
    await mkdir(join(operationDir(dir), "tmp"), { recursive: true })
    await writeFile(join(operationDir(dir), "tmp", "half-written.tmp"), '{"step_seq":4,"record_dig')
    await writeFile(join(operationDir(dir), "records", "000000000004.rec.partial"), '{"step_seq":4')
    await writeFile(join(operationDir(dir), "records", "notes.txt"), "scratch")

    const reopened = new FileKernelJournal(dir)
    const entries = await reopened.readFrom(OP)
    expect(entries.map(entry => entry.step_seq)).toEqual([0, 1, 2, 3])
    expect(entries.map(entry => entry.record_digest)).toEqual(digests)
    expect(await reopened.head(OP)).toEqual({ step_seq: 3, record_digest: "d3" })
    // The chain still accepts its next record, so residue did not poison the CAS position either.
    await reopened.compareAndAppend(OP, "d3", record(4, "d4"))
    expect(await reopened.head(OP)).toEqual({ step_seq: 4, record_digest: "d4" })
  })

  it("reopens installed and acknowledged checkpoints", async () => {
    const journal = new FileKernelJournal(dir)
    const digests = await seedChain(journal, 2)
    await journal.compareAndInstallCheckpoint(OP, undefined, digests[1], candidate("ck-1", 1))
    await journal.ackCheckpoint(OP, "ck-1")

    const reopened = new FileKernelJournal(dir)
    const latest = await reopened.latestCheckpoint(OP)
    expect(latest?.checkpoint_id).toBe("ck-1")
    expect(latest?.acknowledged).toBe(true)
    expect(latest?.covered_head).toBe("d1")
    expect(latest?.through_step_seq).toBe(1)
    expect(Buffer.from(latest!.checkpoint_bytes).toString("utf8")).toBe("checkpoint-ck-1")
  })

  it("raises an integrity fault when a committed record contradicts its own name", async () => {
    const journal = new FileKernelJournal(dir)
    await seedChain(journal, 1)
    await writeFile(join(operationDir(dir), "records", "000000000002.rec"), JSON.stringify({ step_seq: 7 }))

    await expect(new FileKernelJournal(dir).readFrom(OP)).rejects.toBeInstanceOf(JournalIntegrityError)
  })
})

describe("SessionLog / KernelJournal capability separation", () => {
  async function genesis(operationId = "op-1") {
    return createKernelOperationGenesis({
      abi_version: 2,
      operation_id: operationId,
      initial_scheduler_policy: { max_tokens: 8_000 },
      resolved_runtime_defaults: { max_input_bytes: 16_777_216 },
      default_policy_version: 1,
    })
  }

  async function transaction(previousTransactionDigest: string, stepSeq = 1) {
    return createKernelTransaction({
      operation_id: "op-1",
      step_seq: stepSeq,
      base_generation: stepSeq - 1,
      input: { version: 2, operation_id: "op-1", event_id: `event-${stepSeq}` },
      step: { version: 2, operation_id: "op-1", step_seq: stepSeq, actions: [] },
      previous_transaction_digest: previousTransactionDigest,
    })
  }

  it("exposes the journal as its own capability, not as SessionLog methods", async () => {
    const log = new InMemorySessionLog()
    const operationGenesis = await genesis()
    await log.appendKernelGenesis("s1", operationGenesis)

    // The same durable chain is reachable through the separated capability.
    const head = await log.kernelJournal.head(journalOperationKey("s1", "op-1"))
    expect(head).toEqual({ step_seq: 0, record_digest: operationGenesis.genesis_digest })
  })

  it("numbers journal records and business events in independent sequence spaces", async () => {
    const log = new InMemorySessionLog()
    const operationGenesis = await genesis()

    // Interleave: genesis, event, transaction, event, transaction.
    const genesisReceipt = await log.appendKernelGenesis("s1", operationGenesis)
    const event0 = await log.append("s1", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    const first = await transaction(operationGenesis.genesis_digest)
    const firstReceipt = await log.compareAndAppendKernelTransaction("s1", operationGenesis.genesis_digest, first)
    const event1 = await log.append("s1", { kind: "llm_completed", turn: 0, content: "b", tool_calls: [] })
    const second = await transaction(first.transaction_digest, 2)
    const secondReceipt = await log.compareAndAppendKernelTransaction("s1", first.transaction_digest, second)

    // Business events: a dense 0,1 — journal appends never consumed a business number.
    expect([event0, event1]).toEqual([0, 1])
    expect(await log.latestSeq("s1")).toBe(1)
    expect((await log.read("s1")).map(entry => entry.seq)).toEqual([0, 1])

    // Journal records: their own dense chain positions, unaffected by the interleaved events.
    expect(genesisReceipt.log_seq).toBe(0)
    expect(firstReceipt.log_seq).toBe(1)
    expect(secondReceipt.log_seq).toBe(2)
  })

  it("keeps the same sequence-space split on the file-backed pair", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-session-split-"))
    try {
      const log = new FileSessionLog(dir)
      const operationGenesis = await genesis()
      const genesisReceipt = await log.appendKernelGenesis("sess", operationGenesis)
      const event0 = await log.append("sess", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
      const first = await transaction(operationGenesis.genesis_digest)
      const receipt = await log.compareAndAppendKernelTransaction("sess", operationGenesis.genesis_digest, first)
      const event1 = await log.append("sess", { kind: "llm_completed", turn: 0, content: "b", tool_calls: [] })

      expect([event0, event1]).toEqual([0, 1])
      expect(genesisReceipt.log_seq).toBe(0)
      expect(receipt.log_seq).toBe(1)

      // The projection file holds only business events; the journal lives beside it.
      const reopened = new FileSessionLog(dir)
      expect((await reopened.read("sess")).map(entry => entry.seq)).toEqual([0, 1])
      expect(await reopened.latestSeq("sess")).toBe(1)
      expect(await reopened.kernelTransactionHead("sess", "op-1")).toBe(first.transaction_digest)
      expect(await readdir(dir)).toEqual(expect.arrayContaining(["kernel-journal", "sess.jsonl"]))
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })
})

describe("FileKernelJournal — outbound envelope remount", () => {
  it("survives process remount with identical envelope bytes", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-outbound-"))
    try {
      const envelope = JSON.stringify({
        abi_version: 3,
        operation_id: OP,
        input_id: "input-remount",
        observed_at_ms: "1753747200888",
        input: { kind: "configure_operation", config: {} },
      })
      await new FileKernelJournal(dir).stageOutboundEnvelope(OP, envelope)
      expect(await new FileKernelJournal(dir).readOutboundEnvelope(OP)).toBe(envelope)
      await new FileKernelJournal(dir).clearOutboundEnvelope(OP)
      expect(await new FileKernelJournal(dir).readOutboundEnvelope(OP)).toBeUndefined()
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })
})
