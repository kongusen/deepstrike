import { jest } from "@jest/globals"
import { getKernel } from "../../src/kernel.js"
import type { KernelRuntimeHandle } from "../../src/runtime/kernel-step.js"
import { DurableKernelRebuildRequiredError, durableKernelStep } from "../../src/runtime/kernel-step.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import {
  KernelLogConflictError,
  createKernelTransaction,
  type KernelTransaction,
} from "../../src/runtime/kernel-transaction-log.js"

function plannedStep() {
  return {
    version: 2,
    operation_id: "node-operation-1",
    input_event_id: "node-operation-1-event-1",
    step_seq: 1,
    actions: [{ kind: "call_provider", effect_id: "effect-1", context: {}, tools: [] }],
    observations: [{ kind: "run_started" }],
    faults: [],
  }
}

function fakeRuntime(phases: string[]): KernelRuntimeHandle {
  const step = plannedStep()
  return {
    step: () => JSON.stringify(step),
    prepareStep: inputJson => {
      phases.push("prepare")
      return JSON.stringify({
        status: "prepared",
        base_generation: 0,
        prepare_token: "token-1",
        input: JSON.parse(inputJson),
        step,
      })
    },
    commitPrepared: token => {
      expect(token).toBe("token-1")
      phases.push("commit")
      return JSON.stringify(step)
    },
    abortPrepared: token => {
      expect(token).toBe("token-1")
      phases.push("abort")
    },
    snapshot: () => JSON.stringify({
      snapshot_version: 2,
      abi_version: 2,
      initial_policy: {
        max_tokens: 8_000,
        max_turns: 25,
        max_total_tokens: "0",
      },
      lifecycle: "created",
      next_step_seq: 1,
      snapshot_input_limit: 10_000,
      max_input_bytes: 16_777_216,
      snapshot_journal_bytes_limit: 67_108_864,
      accepted_input_bytes: 0,
      accepted_inputs: [],
    }),
    restore: () => undefined,
    diagnostics: () => "{}",
    isTerminal: () => false,
    turn: () => 0,
    recoveryContentBytes: () => 1_024,
    render: () => ({ systemText: "", systemStable: "", systemKnowledge: "", turns: [] }),
    drainNewMessages: () => [],
    preservedRefs: () => [],
  }
}

describe("durableKernelStep", () => {
  it("publishes the committed step only after genesis and transaction durability", async () => {
    const phases: string[] = []
    class OrderedLog extends InMemorySessionLog {
      override async appendKernelGenesis(...args: Parameters<InMemorySessionLog["appendKernelGenesis"]>) {
        phases.push("genesis")
        return super.appendKernelGenesis(...args)
      }
      override async compareAndAppendKernelTransaction(
        ...args: Parameters<InMemorySessionLog["compareAndAppendKernelTransaction"]>
      ) {
        phases.push("durable_append")
        return super.compareAndAppendKernelTransaction(...args)
      }
    }

    const step = await durableKernelStep(
      fakeRuntime(phases),
      new OrderedLog(),
      "session",
      { kind: "start_run", task: { goal: "test", criteria: [] } },
    )

    expect(step.actions).toHaveLength(1)
    expect(phases).toEqual(["genesis", "prepare", "durable_append", "commit"])
  })

  it("each runtime mints a process-unique operation identity (no counter collision across restarts)", async () => {
    // The durable kernel log keys genesis/transaction chains by (sessionId, operationId) and
    // OUTLIVES the process (e.g. a Postgres SessionLog). The old module-level counter restarted
    // at `node-operation-1` in every process, so a restarted host re-entered yesterday's chain on
    // the same session: genesis digest conflict (different policy) or step_seq successor violation
    // (same policy) — either way the run died. Identity must be random per runtime, never ordinal.
    const seenOperationIds: string[] = []
    class CapturingLog extends InMemorySessionLog {
      override async appendKernelGenesis(...args: Parameters<InMemorySessionLog["appendKernelGenesis"]>) {
        seenOperationIds.push(args[1].operation_id)
        return super.appendKernelGenesis(...args)
      }
    }

    // Same persistent log + same sessionId, two runtime instances (the restart shape): both
    // must commit — each starts its own chain instead of resuming the old one by accident.
    const log = new CapturingLog()
    await durableKernelStep(fakeRuntime([]), log, "persistent-session", {
      kind: "start_run",
      task: { goal: "before restart", criteria: [] },
    })
    await durableKernelStep(fakeRuntime([]), log, "persistent-session", {
      kind: "start_run",
      task: { goal: "after restart", criteria: [] },
    })

    expect(seenOperationIds).toHaveLength(2)
    expect(seenOperationIds[0]).not.toBe(seenOperationIds[1])
    for (const id of seenOperationIds) {
      expect(id).toMatch(/^node-operation-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/)
    }
  })

  it("aborts the prepared transition and publishes nothing when durable append fails", async () => {
    const phases: string[] = []
    class FailingLog extends InMemorySessionLog {
      override async compareAndAppendKernelTransaction(): Promise<never> {
        phases.push("durable_append")
        throw new Error("disk unavailable")
      }
    }

    await expect(durableKernelStep(
      fakeRuntime(phases),
      new FailingLog(),
      "session",
      { kind: "start_run", task: { goal: "test", criteria: [] } },
    )).rejects.toThrow("disk unavailable")
    expect(phases).toEqual(["prepare", "durable_append", "abort"])
    expect(phases).not.toContain("commit")
  })
})

/* ------------------------------------------------------------------ *
 * §8.3 rows 4/6: CAS conflict → abort → rebuild → replay, and the abort
 * boundary that sits strictly *before* the durable append.
 * ------------------------------------------------------------------ */

const POLICY = { max_tokens: 8_000, max_turns: 25, max_total_tokens: "0" }
const DEFAULTS = {
  snapshot_version: 2,
  snapshot_input_limit: 10_000,
  max_input_bytes: 16_777_216,
  snapshot_journal_bytes_limit: 67_108_864,
}

/**
 * The one deterministic transition function of these tests. The fake runtime plans with it and the
 * simulated second writer records with it, so a rebuild replay is genuinely digest-verified rather
 * than trivially accepted.
 */
function plannedStepFor(input: Record<string, unknown>, stepSeq: number): Record<string, unknown> {
  return {
    version: 2,
    operation_id: String(input.operation_id),
    input_event_id: String(input.event_id),
    step_seq: stepSeq,
    actions: [{ kind: "call_provider", effect_id: `effect-${stepSeq}`, context: {}, tools: [] }],
    observations: [{ kind: "accepted", turn: stepSeq }],
    faults: [],
  }
}

/** A runtime whose state is its accepted-input journal, so snapshot/restore/rebuild are real. */
function statefulRuntime(options: {
  phases?: string[]
  commitThrows?: boolean
} = {}): KernelRuntimeHandle {
  const phases = options.phases ?? []
  let accepted: Array<{ event_id: string }> = []
  let prepared: { token: string; input: Record<string, unknown>; step: Record<string, unknown> } | undefined
  let tokens = 0
  return {
    step: () => {
      throw new Error("the durable path never uses the direct step primitive")
    },
    prepareStep: inputJson => {
      const input = JSON.parse(inputJson) as Record<string, unknown>
      const stepSeq = accepted.length + 1
      prepared = { token: `token-${(tokens += 1)}`, input, step: plannedStepFor(input, stepSeq) }
      phases.push(`prepare:${stepSeq}`)
      return JSON.stringify({
        status: "prepared",
        base_generation: accepted.length,
        prepare_token: prepared.token,
        input,
        step: prepared.step,
      })
    },
    commitPrepared: token => {
      if (!prepared || prepared.token !== token) throw new Error("invalid prepare token")
      if (options.commitThrows) {
        phases.push("commit_failed")
        throw new Error("kernel commit failed")
      }
      const step = prepared.step
      accepted.push({ event_id: String(prepared.input.event_id) })
      prepared = undefined
      phases.push(`commit:${accepted.length}`)
      return JSON.stringify(step)
    },
    abortPrepared: token => {
      if (!prepared || prepared.token !== token) throw new Error("invalid prepare token")
      prepared = undefined
      phases.push("abort")
    },
    snapshot: () => JSON.stringify({
      ...DEFAULTS,
      abi_version: 2,
      initial_policy: POLICY,
      lifecycle: "created",
      next_step_seq: accepted.length + 1,
      accepted_input_bytes: 0,
      accepted_inputs: accepted,
    }),
    restore: snapshotJson => {
      const snapshot = JSON.parse(snapshotJson) as { accepted_inputs: Array<{ event_id: string }> }
      accepted = [...snapshot.accepted_inputs]
      prepared = undefined
      phases.push(`restore:${accepted.length}`)
    },
    diagnostics: () => "{}",
    isTerminal: () => false,
    turn: () => 0,
    recoveryContentBytes: () => 1_024,
    render: () => ({ systemText: "", systemStable: "", systemKnowledge: "", turns: [] }),
    drainNewMessages: () => [],
    preservedRefs: () => [],
  }
}

const startRun = { kind: "start_run", task: { goal: "test", criteria: [] } }

describe("durableKernelStep CAS conflict rebuild/retry", () => {
  it("rebuilds and replays the same input, producing an identical step_digest", async () => {
    // A transient conflict that leaves the head where it was: after the rebuild the runtime is back
    // at the exact state the first attempt planned from, so the replayed input must reproduce the
    // rejected candidate byte for byte (Task 7b acceptance #3).
    const attempts: KernelTransaction[] = []
    class ConflictOnceLog extends InMemorySessionLog {
      private conflictsLeft = 1
      override async compareAndAppendKernelTransaction(
        ...args: Parameters<InMemorySessionLog["compareAndAppendKernelTransaction"]>
      ) {
        attempts.push(args[2])
        if (this.conflictsLeft > 0) {
          this.conflictsLeft -= 1
          throw new KernelLogConflictError("kernel transaction head changed before compare-and-append")
        }
        return super.compareAndAppendKernelTransaction(...args)
      }
    }

    const phases: string[] = []
    const log = new ConflictOnceLog()
    const step = await durableKernelStep(statefulRuntime({ phases }), log, "session", startRun)

    expect(step.step_seq).toBe(1)
    expect(step.actions).toHaveLength(1)
    expect(attempts).toHaveLength(2)
    expect(attempts[1].step_digest).toBe(attempts[0].step_digest)
    expect(attempts[1].input_digest).toBe(attempts[0].input_digest)
    expect(attempts[1].transaction_digest).toBe(attempts[0].transaction_digest)
    // abort belongs to the pre-append window; the rebuild resets to genesis (0 records) and replays.
    expect(phases).toEqual(["prepare:1", "abort", "restore:0", "prepare:1", "commit:1"])

    const durable = await log.readKernelTransactions("session", step.operation_id)
    expect(durable).toHaveLength(1)
    expect(durable[0].transaction.step_digest).toBe(attempts[0].step_digest)
  })

  it("folds a second writer's record and retries the input against the advanced head", async () => {
    // The real race: another writer commits between our head read and our CAS. The run must not die;
    // it re-folds the authoritative stream (production caller of `rebuildKernelRuntime`) and replays.
    class SecondWriterLog extends InMemorySessionLog {
      foreign?: KernelTransaction
      override async compareAndAppendKernelTransaction(
        ...args: Parameters<InMemorySessionLog["compareAndAppendKernelTransaction"]>
      ) {
        const [sessionId, expectedHead, transaction] = args
        if (!this.foreign) {
          const foreignInput = {
            version: 2,
            operation_id: transaction.operation_id,
            event_id: `${transaction.operation_id}-foreign-1`,
            observed_at_ms: 1,
            event: { kind: "noop" },
          }
          this.foreign = await createKernelTransaction({
            operation_id: transaction.operation_id,
            step_seq: 1,
            base_generation: 0,
            input: foreignInput,
            step: plannedStepFor(foreignInput, 1),
            previous_transaction_digest: expectedHead,
          })
          await super.compareAndAppendKernelTransaction(sessionId, expectedHead, this.foreign)
        }
        return super.compareAndAppendKernelTransaction(...args)
      }
    }

    const phases: string[] = []
    const log = new SecondWriterLog()
    const step = await durableKernelStep(statefulRuntime({ phases }), log, "session", startRun)

    expect(step.step_seq).toBe(2)
    // prepare@1 → lost CAS → abort → reset to genesis → replay the foreign record → replay our input.
    expect(phases).toEqual([
      "prepare:1",
      "abort",
      "restore:0",
      "prepare:1",
      "commit:1",
      "prepare:2",
      "commit:2",
    ])

    const durable = await log.readKernelTransactions("session", step.operation_id)
    expect(durable.map(entry => entry.transaction.step_seq)).toEqual([1, 2])
    expect(durable[0].transaction.transaction_digest).toBe(log.foreign!.transaction_digest)
  })

  it("retries a CAS conflict exactly once and then surfaces it", async () => {
    // Bounded on purpose: a second consecutive conflict is sustained contention, not a race, and an
    // unbounded rebuild/retry loop would livelock instead of reporting the host identity error.
    let attempts = 0
    class AlwaysConflictLog extends InMemorySessionLog {
      override async compareAndAppendKernelTransaction(): Promise<never> {
        attempts += 1
        throw new KernelLogConflictError("kernel transaction head changed before compare-and-append")
      }
    }

    const phases: string[] = []
    await expect(
      durableKernelStep(statefulRuntime({ phases }), new AlwaysConflictLog(), "session", startRun),
    ).rejects.toBeInstanceOf(KernelLogConflictError)

    expect(attempts).toBe(2)
    expect(phases).toEqual(["prepare:1", "abort", "restore:0", "prepare:1", "abort"])
  })

  it("never aborts after the append succeeds: a failing commit demands a rebuild instead", async () => {
    // Task 7b acceptance #2 / §8.3 row 6. The record is durable, so `abortPrepared` is off the table
    // — the runtime is what gets discarded.
    const phases: string[] = []
    const runtime = statefulRuntime({ phases, commitThrows: true })
    const abortSpy = jest.fn(runtime.abortPrepared)
    runtime.abortPrepared = abortSpy
    const appended: KernelTransaction[] = []
    class RecordingLog extends InMemorySessionLog {
      override async compareAndAppendKernelTransaction(
        ...args: Parameters<InMemorySessionLog["compareAndAppendKernelTransaction"]>
      ) {
        appended.push(args[2])
        return super.compareAndAppendKernelTransaction(...args)
      }
    }
    const log = new RecordingLog()

    const error = await durableKernelStep(runtime, log, "session", startRun).catch((e: unknown) => e)

    expect(error).toBeInstanceOf(DurableKernelRebuildRequiredError)
    expect((error as Error).message).toMatch(/rebuild it from the journal/)
    expect((error as { cause?: Error }).cause?.message).toBe("kernel commit failed")
    expect(abortSpy).not.toHaveBeenCalled()
    expect(phases).toEqual(["prepare:1", "commit_failed"])

    // The durable record stands — a failed commit must not undo it.
    expect(appended).toHaveLength(1)
    const durable = await log.readKernelTransactions("session", appended[0].operation_id)
    expect(durable).toHaveLength(1)
    expect(durable[0].transaction.transaction_digest).toBe(appended[0].transaction_digest)
  })

  it("fails closed on every later transition once a runtime has been discarded", async () => {
    const runtime = statefulRuntime({ commitThrows: true })
    const log = new InMemorySessionLog()

    const first = await durableKernelStep(runtime, log, "session", startRun).catch((e: unknown) => e)
    const second = await durableKernelStep(runtime, log, "session", startRun).catch((e: unknown) => e)

    expect(first).toBeInstanceOf(DurableKernelRebuildRequiredError)
    // Same instance: the discard is remembered, not re-derived from a second doomed attempt.
    expect(second).toBe(first)
  })

  it("re-folds a LIVE kernel runtime in place and replays the input identically", async () => {
    // The same loop against the real ABI: `restore()` swaps the binding's inner runtime, so the
    // handle the host holds survives the rebuild, and the deterministic replay must reproduce the
    // rejected candidate exactly.
    const kernel = getKernel()
    const runtime = new kernel.KernelRuntime({ maxTokens: 8_000, maxTurns: 25 })
    const attempts: KernelTransaction[] = []
    class ConflictOnSecondStepLog extends InMemorySessionLog {
      conflicted = false
      override async compareAndAppendKernelTransaction(
        ...args: Parameters<InMemorySessionLog["compareAndAppendKernelTransaction"]>
      ) {
        attempts.push(args[2])
        if (args[2].step_seq === 2 && !this.conflicted) {
          this.conflicted = true
          throw new KernelLogConflictError("kernel transaction head changed before compare-and-append")
        }
        return super.compareAndAppendKernelTransaction(...args)
      }
    }

    const log = new ConflictOnSecondStepLog()
    await durableKernelStep(runtime, log, "live-session", { kind: "set_tools", tools: [] })
    const step = await durableKernelStep(runtime, log, "live-session", {
      kind: "start_run",
      task: { goal: "rebuild after a lost CAS race", criteria: [] },
    })

    expect(log.conflicted).toBe(true)
    expect(step.step_seq).toBe(2)
    expect(step.faults ?? []).toHaveLength(0)
    expect(step.actions[0]?.kind).toBe("call_provider")
    // [step 1, step 2 rejected, step 2 retried after the rebuild]
    expect(attempts.map(a => a.step_seq)).toEqual([1, 2, 2])
    expect(attempts[2].step_digest).toBe(attempts[1].step_digest)
    expect(attempts[2].transaction_digest).toBe(attempts[1].transaction_digest)

    const durable = await log.readKernelTransactions("live-session", step.operation_id)
    expect(durable.map(entry => entry.transaction.step_seq)).toEqual([1, 2])

    // The rebuilt runtime is the same live handle and is still usable for the next transition.
    const next = await durableKernelStep(runtime, log, "live-session", {
      kind: "provider_error",
      effect_id: String(step.actions[0]?.effect_id),
      message: "transient",
      retryable: false,
    })
    expect(next.step_seq).toBe(3)
  })
})
