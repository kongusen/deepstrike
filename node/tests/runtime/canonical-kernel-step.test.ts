import { jest } from "@jest/globals"
import type {
  CanonicalCommit,
  CanonicalKernelInstance,
  CanonicalPreparation,
  CanonicalRestoreCost,
} from "../../src/kernel.js"
import {
  CanonicalKernelHost,
  CanonicalKernelRebuildRequiredError,
} from "../../src/runtime/canonical-kernel-step.js"
import {
  InMemoryKernelJournal,
  JournalCasConflictError,
  type CheckpointCandidate,
  type JournalAppendReceipt,
  type JournalRecordInput,
} from "../../src/runtime/kernel-journal.js"

const OPERATION_ID = "node-operation-run-1"

function prepared(
  inputJson: string,
  stepSeq: number,
  expectedHead: string | undefined,
  recordDigest: string,
): CanonicalPreparation {
  return {
    status: "prepared",
    prepareToken: `token-${stepSeq}`,
    stepSeq: String(stepSeq),
    ...(expectedHead ? { expectedHead } : {}),
    recordDigest,
    recordBytes: Buffer.from(`record:${inputJson}`, "utf8"),
    plannedStepJson: JSON.stringify({ disposition: { kind: "effects" } }),
  }
}

function fakeKernel(phases: string[]): CanonicalKernelInstance {
  let nextStep = 0
  let head: string | undefined
  let checkpointThrough = 0
  return {
    prepare(inputJson: string): CanonicalPreparation {
      phases.push(`prepare:${inputJson}`)
      return prepared(inputJson, nextStep, head, `digest-${nextStep}`)
    },
    commit(token: string, appendedHead: string): CanonicalCommit {
      phases.push(`commit:${token}:${appendedHead}`)
      head = appendedHead
      checkpointThrough = nextStep
      nextStep += 1
      return {
        stepSeq: String(nextStep - 1),
        recordDigest: appendedHead,
        plannedStepJson: JSON.stringify({ disposition: { kind: "effects" } }),
      }
    },
    abort(token: string): void {
      phases.push(`abort:${token}`)
    },
    checkpointCandidate() {
      phases.push("checkpoint_candidate")
      return {
        checkpointBytes: Buffer.from(`checkpoint-${checkpointThrough}`, "utf8"),
        throughStepSeq: String(checkpointThrough),
        coveredHead: head!,
        stateDigest: `state-${checkpointThrough}`,
        ackToken: `checkpoint-${checkpointThrough}`,
      }
    },
    checkpointRebase() {
      throw new Error("not used")
    },
    ackCheckpoint(throughStepSeq: string, coveredHead: string): void {
      phases.push(`core_ack:${throughStepSeq}:${coveredHead}`)
    },
    restore(_checkpointBytes: Buffer | undefined, recordBytes: Buffer[]): CanonicalRestoreCost {
      phases.push(`restore:${recordBytes.map(bytes => bytes.toString("utf8")).join("|")}`)
      nextStep = recordBytes.length
      head = recordBytes.length > 0 ? `digest-${recordBytes.length - 1}` : undefined
      return {
        recordsBeforeCheckpoint: "0",
        tailInputsReplayed: String(recordBytes.length),
        recordsAfterCheckpoint: "0",
        bytesRead: String(recordBytes.reduce((sum, bytes) => sum + bytes.byteLength, 0)),
      }
    },
    lifecycle: () => "running",
    pendingEffectsJson: () => "[]",
    terminalJson: () => undefined,
  }
}

describe("CanonicalKernelHost", () => {
  it("publishes a step only after core record bytes are durably appended", async () => {
    const phases: string[] = []
    class OrderedJournal extends InMemoryKernelJournal {
      override async compareAndAppend(
        operationId: string,
        expectedHead: string | undefined,
        record: JournalRecordInput,
      ): Promise<JournalAppendReceipt> {
        phases.push(`append:${Buffer.from(record.record_bytes).toString("utf8")}`)
        return super.compareAndAppend(operationId, expectedHead, record)
      }
    }
    const host = new CanonicalKernelHost(fakeKernel(phases), new OrderedJournal(), OPERATION_ID)

    await host.transition(
      { kind: "configure_operation", config: { host_effect_support: { supported: ["call_provider"] } } },
      { inputId: "input-1", observedAtMs: "1753747200000" },
    )

    expect(phases.map(phase => phase.split(":")[0])).toEqual(["prepare", "append", "commit"])
    const stored = await host.journal.readFrom(OPERATION_ID)
    expect(Buffer.from(stored[0].record_bytes).toString("utf8")).toContain(
      '"observed_at_ms":"1753747200000"',
    )
  })

  it("aborts on a CAS loss, restores the winner, and retries the exact same envelope bytes once", async () => {
    const phases: string[] = []
    const kernel = fakeKernel(phases)
    const journal = new InMemoryKernelJournal()
    const originalAppend = journal.compareAndAppend.bind(journal)
    let first = true
    jest.spyOn(journal, "compareAndAppend").mockImplementation(async (...args) => {
      if (first) {
        first = false
        await originalAppend(
          OPERATION_ID,
          undefined,
          {
            step_seq: 0,
            record_digest: "digest-0",
            record_bytes: args[2].record_bytes,
          },
        )
        throw new JournalCasConflictError("winner committed")
      }
      return originalAppend(...args)
    })
    const host = new CanonicalKernelHost(kernel, journal, OPERATION_ID)

    await host.transition(
      { kind: "configure_operation", config: { host_effect_support: { supported: ["call_provider"] } } },
      { inputId: "input-stable", observedAtMs: "1753747200001" },
    )

    const preparedInputs = phases
      .filter(phase => phase.startsWith("prepare:"))
      .map(phase => phase.slice("prepare:".length))
    expect(preparedInputs).toHaveLength(2)
    expect(preparedInputs[1]).toBe(preparedInputs[0])
    expect(phases).toContain("abort:token-0")
    expect(phases.some(phase => phase.startsWith("restore:"))).toBe(true)
  })

  it("rebuilds after a durable append when commit loses its response", async () => {
    const phases: string[] = []
    const kernel = fakeKernel(phases)
    const originalCommit = kernel.commit.bind(kernel)
    let failOnce = true
    kernel.commit = (token, appendedHead) => {
      if (failOnce) {
        failOnce = false
        phases.push("commit_lost")
        throw new Error("response lost")
      }
      return originalCommit(token, appendedHead)
    }
    const host = new CanonicalKernelHost(kernel, new InMemoryKernelJournal(), OPERATION_ID)

    const error = await host.transition(
      { kind: "configure_operation", config: { host_effect_support: { supported: ["call_provider"] } } },
      { inputId: "input-commit-loss", observedAtMs: "1753747200002" },
    ).catch((cause: unknown) => cause)

    expect(error).toBeInstanceOf(CanonicalKernelRebuildRequiredError)
    expect(phases).not.toContain("abort:token-0")
    expect(phases.some(phase => phase.startsWith("restore:"))).toBe(true)
  })

  it("installs, durably acknowledges, core-acknowledges, then prunes a checkpoint", async () => {
    const phases: string[] = []
    const kernel = fakeKernel(phases)
    const originalCommit = kernel.commit.bind(kernel)
    kernel.commit = (token, appendedHead) => ({
      ...originalCommit(token, appendedHead),
      checkpointAdviceJson: JSON.stringify({ through_step_seq: "0" }),
    })
    class OrderedJournal extends InMemoryKernelJournal {
      override async compareAndInstallCheckpoint(
        operationId: string,
        previousCheckpointId: string | undefined,
        coveredHead: string,
        checkpoint: CheckpointCandidate,
      ) {
        phases.push("install")
        return super.compareAndInstallCheckpoint(
          operationId,
          previousCheckpointId,
          coveredHead,
          checkpoint,
        )
      }
      override async ackCheckpoint(operationId: string, checkpointId: string) {
        phases.push("journal_ack")
        return super.ackCheckpoint(operationId, checkpointId)
      }
      override async pruneAckedPrefix(operationId: string) {
        phases.push("prune")
        return super.pruneAckedPrefix(operationId)
      }
    }
    const host = new CanonicalKernelHost(kernel, new OrderedJournal(), OPERATION_ID)

    await host.transition(
      { kind: "configure_operation", config: { host_effect_support: { supported: ["call_provider"] } } },
      { inputId: "input-checkpoint", observedAtMs: "1753747200003" },
    )

    expect(phases.filter(phase =>
      ["checkpoint_candidate", "install", "journal_ack", "prune"].includes(phase) ||
      phase.startsWith("core_ack:"),
    )).toEqual([
      "checkpoint_candidate",
      "install",
      "journal_ack",
      "core_ack:0:digest-0",
      "prune",
    ])
  })
})
