import { jest } from "@jest/globals"
import type {
  CanonicalCommit,
  CanonicalKernel as CanonicalKernelInstance,
  CanonicalPreparation,
  CanonicalRestoreCost,
} from "@deepstrike/wasm-kernel"
import {
  CanonicalKernelHost,
  CanonicalKernelRebuildRequiredError,
  CanonicalRunnerRuntime,
} from "../src/runtime/canonical-kernel-step.js"
import {
  InMemoryKernelJournal,
  type JournalAppendReceipt,
  type JournalRecordInput,
} from "../src/runtime/kernel-journal.js"

const OPERATION_ID = "wasm-operation-run-1"
const encoder = new TextEncoder()
const decoder = new TextDecoder()

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
    recordBytes: encoder.encode(`record:${inputJson}`),
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
        checkpointBytes: encoder.encode(`checkpoint-${checkpointThrough}`),
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
    restore(_checkpointBytes: Uint8Array | undefined, recordBytes: Uint8Array[]): CanonicalRestoreCost {
      phases.push(`restore:${recordBytes.map(bytes => decoder.decode(bytes)).join("|")}`)
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
        phases.push(`append:${decoder.decode(record.record_bytes)}`)
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
    expect(decoder.decode(stored[0].record_bytes)).toContain('"observed_at_ms":"1753747200000"')
    expect(await host.journal.readOutboundEnvelope(OPERATION_ID)).toBeUndefined()
  })

  it("publishes the committed transition even when the advised checkpoint fails", async () => {
    const phases: string[] = []
    const kernel = fakeKernel(phases)
    const originalCommit = kernel.commit.bind(kernel)
    kernel.commit = (token, appendedHead) => ({
      ...originalCommit(token, appendedHead),
      checkpointAdviceJson: JSON.stringify({ through_step_seq: "0" }),
    })
    const journal = new InMemoryKernelJournal()
    jest.spyOn(journal, "compareAndInstallCheckpoint")
      .mockRejectedValueOnce(new Error("journal io: storage 503") as never)
    const host = new CanonicalKernelHost(kernel, journal, OPERATION_ID)

    // The record is durable and commit already returned: a failing §12.3 checkpoint
    // is deferred housekeeping (the next advice or the checkpoint_required gate
    // retries it), not a failed commit.
    const transition = await host.transition(
      { kind: "configure_operation", config: { host_effect_support: { supported: ["call_provider"] } } },
      { inputId: "input-checkpoint-io", observedAtMs: "1753747200004" },
    )

    expect(transition.replayed).toBe(false)
    expect(transition.checkpointAdvice).toBeDefined()
    expect(transition.checkpointFailure).toContain("storage 503")
    // Not misdiagnosed as a lost commit: no rebuild happened.
    expect(phases.some(phase => phase.startsWith("restore:"))).toBe(false)
    // The committed step is durable and the stage is clear for the next input.
    expect(await journal.readOutboundEnvelope(OPERATION_ID)).toBeUndefined()
  })
})

describe("CanonicalRunnerRuntime rebuild recovery", () => {
  function commitLossKernel(phases: string[]): CanonicalKernelInstance {
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
    return kernel
  }

  it("continues on the rebuilt kernel instead of failing the run when a commit response is lost", async () => {
    const phases: string[] = []
    const journal = new InMemoryKernelJournal()
    const runtime = new CanonicalRunnerRuntime(commitLossKernel(phases), journal, OPERATION_ID, {
      maxContextTokens: 8_192,
    })

    await runtime.startAgent({ goal: "recover across a lost commit response" })

    expect(phases.some(phase => phase.startsWith("restore:"))).toBe(true)
    const stored = await journal.readFrom(OPERATION_ID)
    expect(stored).toHaveLength(2)
    expect(runtime.drainHostObservations().some(
      observation => observation.kind === "kernel_rebuilt",
    )).toBe(true)
  })

  it("surfaces a deferred checkpoint failure as a host observation", async () => {
    const phases: string[] = []
    const kernel = fakeKernel(phases)
    const originalCommit = kernel.commit.bind(kernel)
    kernel.commit = (token, appendedHead) => ({
      ...originalCommit(token, appendedHead),
      checkpointAdviceJson: JSON.stringify({ through_step_seq: "0" }),
    })
    const journal = new InMemoryKernelJournal()
    jest.spyOn(journal, "compareAndInstallCheckpoint")
      .mockRejectedValueOnce(new Error("journal io: storage 503") as never)
    const runtime = new CanonicalRunnerRuntime(kernel, journal, OPERATION_ID, {
      maxContextTokens: 8_192,
    })

    await runtime.startAgent({ goal: "observe deferred checkpoint" })

    const observations = runtime.drainHostObservations()
    const deferred = observations.find(observation => observation.kind === "checkpoint_deferred")
    expect(deferred).toBeDefined()
    expect(String(deferred?.reason)).toContain("storage 503")
  })

  it("still fails the run when the journal rebuild itself fails", async () => {
    const phases: string[] = []
    const journal = new InMemoryKernelJournal()
    jest.spyOn(journal, "recordsAfter").mockRejectedValue(new Error("journal unreadable") as never)
    const runtime = new CanonicalRunnerRuntime(commitLossKernel(phases), journal, OPERATION_ID, {
      maxContextTokens: 8_192,
    })

    await expect(runtime.startAgent({ goal: "fatal" }))
      .rejects.toThrow(CanonicalKernelRebuildRequiredError)
  })
})

describe("CanonicalRunnerRuntime provider stop reasons", () => {
  it("normalizes an unknown non-empty provider stop reason to other before the kernel transition", async () => {
    const phases: string[] = []
    const kernel = fakeKernel(phases)
    const inputs: Record<string, unknown>[] = []
    const originalPrepare = kernel.prepare.bind(kernel)
    kernel.prepare = (inputJson: string) => {
      inputs.push(JSON.parse(inputJson) as Record<string, unknown>)
      return originalPrepare(inputJson)
    }
    const runtime = new CanonicalRunnerRuntime(kernel, new InMemoryKernelJournal(), OPERATION_ID, {
      maxContextTokens: 8_192,
    })

    await runtime.startAgent({ goal: "normalize provider termination" })
    await runtime.applyHostEvent({
      kind: "provider_result",
      effect_id: "provider-1",
      message: { role: "assistant", content: "done" },
      stop_reason: "vendor_new_reason",
    })

    const providerResult = inputs.at(-1) as { input?: { outcome?: { result?: { outcome?: Record<string, unknown> } } } }
    expect(providerResult.input?.outcome?.result?.outcome?.stop_reason).toBe("other")
  })

  it("omits an empty provider stop reason", async () => {
    const phases: string[] = []
    const kernel = fakeKernel(phases)
    const inputs: Record<string, unknown>[] = []
    const originalPrepare = kernel.prepare.bind(kernel)
    kernel.prepare = (inputJson: string) => {
      inputs.push(JSON.parse(inputJson) as Record<string, unknown>)
      return originalPrepare(inputJson)
    }
    const runtime = new CanonicalRunnerRuntime(kernel, new InMemoryKernelJournal(), OPERATION_ID, {
      maxContextTokens: 8_192,
    })

    await runtime.startAgent({ goal: "omit empty provider termination" })
    await runtime.applyHostEvent({
      kind: "provider_result",
      effect_id: "provider-2",
      message: { role: "assistant", content: "done" },
      stop_reason: "",
    })

    const providerResult = inputs.at(-1) as { input?: { outcome?: { result?: { outcome?: Record<string, unknown> } } } }
    expect(providerResult.input?.outcome?.result?.outcome).not.toHaveProperty("stop_reason")
  })
})
