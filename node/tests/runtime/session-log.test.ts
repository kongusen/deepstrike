import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { InMemorySessionLog, FileSessionLog } from "../../src/runtime/session-log.js"
import {
  KernelLogConflictError,
  KernelLogIntegrityError,
  createKernelOperationGenesis,
  createKernelTransaction,
  kernelRecordDigest,
  verifyKernelTransactionStream,
} from "../../src/runtime/kernel-transaction-log.js"

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

describe("InMemorySessionLog", () => {
  it("pins the cross-SDK canonical digest codec", () => {
    expect(kernelRecordDigest({ z: 1, a: [true, "雪"] })).toBe(
      "74ffaa09c9570f87244813a5b15514369f7b1a8996e3e80017585b4df246c1f7",
    )
    expect(kernelRecordDigest({ ratio: 0.5 })).toBe(
      "7ae3311a2b33b26525cf688e72ec90df645b018c033d6b9efc23f422af4f8391",
    )
    expect(() => kernelRecordDigest({ ratio: Number.POSITIVE_INFINITY })).toThrow(KernelLogIntegrityError)
  })

  it("validates the complete digest chain and derives a regex-free operation cursor", async () => {
    const operationGenesis = await genesis()
    const first = await transaction(operationGenesis.genesis_digest)
    const second = await transaction(first.transaction_digest, 2)

    expect(verifyKernelTransactionStream(operationGenesis, [first, second])).toEqual({
      operation_id: "op-1",
      next_event_sequence: 3,
      next_step_seq: 3,
      transaction_head_digest: second.transaction_digest,
    })
    expect(() => verifyKernelTransactionStream(operationGenesis, [second, first])).toThrow(
      KernelLogIntegrityError,
    )
  })

  it("fences authoritative transactions without coupling projection appends", async () => {
    const log = new InMemorySessionLog()
    const operationGenesis = await genesis()
    const genesisReceipt = await log.appendKernelGenesis("s1", operationGenesis)
    expect(genesisReceipt.genesis_digest).toBe(operationGenesis.genesis_digest)

    await log.append("s1", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    expect(await log.kernelTransactionHead("s1", "op-1")).toBe(operationGenesis.genesis_digest)

    const first = await transaction(operationGenesis.genesis_digest)
    const firstReceipt = await log.compareAndAppendKernelTransaction(
      "s1",
      operationGenesis.genesis_digest,
      first,
    )
    expect(firstReceipt.transaction_digest).toBe(first.transaction_digest)
    expect(await log.kernelTransactionHead("s1", "op-1")).toBe(first.transaction_digest)

    const secondGenesis = await genesis("op-2")
    await log.appendKernelGenesis("s1", secondGenesis)
    expect(await log.kernelTransactionHead("s1", "op-2")).toBe(secondGenesis.genesis_digest)
    expect(await log.kernelTransactionHead("s1", "op-1")).toBe(first.transaction_digest)

    const stale = await transaction(operationGenesis.genesis_digest, 2)
    await expect(
      log.compareAndAppendKernelTransaction("s1", operationGenesis.genesis_digest, stale),
    ).rejects.toBeInstanceOf(KernelLogConflictError)
    const skipped = await transaction(first.transaction_digest, 3)
    await expect(
      log.compareAndAppendKernelTransaction("s1", first.transaction_digest, skipped),
    ).rejects.toBeInstanceOf(KernelLogIntegrityError)
    expect(await log.readKernelTransactions("s1", "op-1")).toEqual([{ log_seq: firstReceipt.log_seq, transaction: first }])
  })

  it("rejects transaction payload tampering before append", async () => {
    const log = new InMemorySessionLog()
    const operationGenesis = await genesis()
    await log.appendKernelGenesis("s1", operationGenesis)
    const valid = await transaction(operationGenesis.genesis_digest)
    const tampered = { ...valid, input: { ...valid.input, event_id: "tampered" } }

    await expect(
      log.compareAndAppendKernelTransaction("s1", operationGenesis.genesis_digest, tampered),
    ).rejects.toBeInstanceOf(KernelLogIntegrityError)
    expect(await log.readKernelTransactions("s1", "op-1")).toEqual([])
  })

  it("append returns monotonic seq starting at 0", async () => {
    const log = new InMemorySessionLog()
    const s0 = await log.append("s1", { kind: "run_started", run_id: "r1", goal: "hi", criteria: [] })
    const s1 = await log.append("s1", {
      kind: "llm_completed",
      turn: 0,
      content: "ok",
      tool_calls: [],
    })
    expect(s0).toBe(0)
    expect(s1).toBe(1)
    expect(await log.latestSeq("s1")).toBe(1)
  })

  it("read filters by fromSeq", async () => {
    const log = new InMemorySessionLog()
    await log.append("s1", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    await log.append("s1", { kind: "llm_completed", turn: 0, content: "b", tool_calls: [] })
    await log.append("s1", { kind: "run_terminal", reason: "completed", turns_used: 1, total_tokens: 10 })

    const tail = await log.read("s1", 1)
    expect(tail).toHaveLength(2)
    expect(tail[0].seq).toBe(1)
    expect(tail[1].event.kind).toBe("run_terminal")
  })

  it("read filters by primitiveFilter", async () => {
    const log = new InMemorySessionLog()
    await log.append("s1", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    await log.append("s1", { kind: "page_out", turn: 0, category: "mm", primitive: "mm", summary: "po" })
    await log.append("s1", { kind: "suspended", turn: 1, category: "sched", primitive: "sched", reason: "sus" })
    await log.append("s1", { kind: "tool_gated", turn: 2, category: "syscall", primitive: "syscall", call_id: "c1", tool: "t1", reason: "gated" })

    const mmEvents = await log.read("s1", 0, "mm")
    expect(mmEvents).toHaveLength(1)
    expect(mmEvents[0].event.kind).toBe("page_out")

    const schedEvents = await log.read("s1", 1, "sched")
    expect(schedEvents).toHaveLength(1)
    expect(schedEvents[0].event.kind).toBe("suspended")

    const syscallEvents = await log.read("s1", 0, "syscall")
    expect(syscallEvents).toHaveLength(1)
    expect(syscallEvents[0].event.kind).toBe("tool_gated")
  })

  it("isolates sessions", async () => {
    const log = new InMemorySessionLog()
    await log.append("a", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    await log.append("b", { kind: "run_started", run_id: "r2", goal: "b", criteria: [] })
    expect((await log.read("a")).length).toBe(1)
    expect((await log.read("b")).length).toBe(1)
  })

  it("latestSeq is -1 for unknown session", async () => {
    const log = new InMemorySessionLog()
    expect(await log.latestSeq("missing")).toBe(-1)
  })
})

describe("FileSessionLog", () => {
  let dir: string

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "ds-session-log-"))
  })

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true })
  })

  it("persists and reloads events", async () => {
    const log = new FileSessionLog(dir)
    await log.append("sess-1", { kind: "run_started", run_id: "r1", goal: "persist", criteria: [] })
    await log.append("sess-1", {
      kind: "tool_completed",
      turn: 1,
      results: [{ call_id: "c1", output: "pong", is_error: false }],
    })

    const log2 = new FileSessionLog(dir)
    const events = await log2.read("sess-1")
    expect(events).toHaveLength(2)
    expect(events[1].event.kind).toBe("tool_completed")
  })

  it("persists the authoritative genesis and transaction substream", async () => {
    const log = new FileSessionLog(dir)
    const operationGenesis = await genesis()
    const genesisReceipt = await log.appendKernelGenesis("sess-kernel", operationGenesis)
    await log.append("sess-kernel", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    const first = await transaction(operationGenesis.genesis_digest)
    const receipt = await log.compareAndAppendKernelTransaction(
      "sess-kernel",
      operationGenesis.genesis_digest,
      first,
    )

    const reopened = new FileSessionLog(dir)
    expect(await reopened.readKernelGenesis("sess-kernel", "op-1")).toEqual(operationGenesis)
    expect(await reopened.readKernelTransactions("sess-kernel", "op-1")).toEqual([
      { log_seq: receipt.log_seq, transaction: first },
    ])
    expect(await reopened.kernelTransactionHead("sess-kernel", "op-1")).toBe(first.transaction_digest)
    expect(genesisReceipt.log_seq).toBe(0)
    expect(receipt.log_seq).toBe(2)
  })

  it("read returns empty for missing session file", async () => {
    const log = new FileSessionLog(dir)
    expect(await log.read("no-such-session")).toEqual([])
  })

  it("serializes concurrent appends within one instance", async () => {
    const log = new FileSessionLog(dir)

    const returned = await Promise.all([
      log.append("sess-concurrent", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] }),
      log.append("sess-concurrent", { kind: "run_started", run_id: "r2", goal: "b", criteria: [] }),
    ])

    expect(returned).toEqual([0, 1])
    expect((await log.read("sess-concurrent")).map(entry => entry.seq)).toEqual([0, 1])
    expect(await log.latestSeq("sess-concurrent")).toBe(1)
  })
})
