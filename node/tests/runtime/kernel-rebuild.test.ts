import type { KernelRuntimeHandle, KernelStepJson } from "../../src/runtime/kernel-step.js"
import { rebuildKernelRuntime } from "../../src/runtime/kernel-rebuild.js"
import {
  KernelLogIntegrityError,
  createKernelOperationGenesis,
  createKernelTransaction,
  type KernelTransaction,
} from "../../src/runtime/kernel-transaction-log.js"

const policy = { max_tokens: 8_000, max_turns: 25, max_total_tokens: "0" }
const defaults = {
  snapshot_version: 2,
  snapshot_input_limit: 10_000,
  max_input_bytes: 16_777_216,
  snapshot_journal_bytes_limit: 67_108_864,
}

function runtime(phases: string[], driftAt = 0): KernelRuntimeHandle {
  let generation = 0
  let prepared: { token: string; step: KernelStepJson } | undefined
  return {
    step: () => "{}",
    prepareStep: inputJson => {
      const input = JSON.parse(inputJson) as Record<string, unknown>
      const stepSeq = generation + 1
      const step: KernelStepJson = {
        version: 2,
        operation_id: String(input.operation_id),
        input_event_id: String(input.event_id),
        step_seq: stepSeq,
        actions: driftAt === stepSeq ? [{ kind: "drift" }] : [],
        observations: [{ kind: `accepted_${stepSeq}` }],
        faults: [],
      }
      prepared = { token: `token-${stepSeq}`, step }
      phases.push(`prepare:${stepSeq}`)
      return JSON.stringify({
        status: "prepared",
        base_generation: generation,
        prepare_token: prepared.token,
        input,
        step,
      })
    },
    commitPrepared: token => {
      if (!prepared || prepared.token !== token) throw new Error("invalid token")
      generation += 1
      phases.push(`commit:${generation}`)
      const step = prepared.step
      prepared = undefined
      return JSON.stringify(step)
    },
    abortPrepared: token => {
      if (!prepared || prepared.token !== token) throw new Error("invalid token")
      phases.push(`abort:${generation + 1}`)
      prepared = undefined
    },
    snapshot: () => JSON.stringify({
      ...defaults,
      abi_version: 2,
      initial_policy: policy,
      lifecycle: "created",
      next_step_seq: 1,
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

async function fixture(): Promise<{
  genesis: Awaited<ReturnType<typeof createKernelOperationGenesis>>
  transactions: KernelTransaction[]
}> {
  const genesis = await createKernelOperationGenesis({
    abi_version: 2,
    operation_id: "operation",
    initial_scheduler_policy: policy,
    resolved_runtime_defaults: defaults,
    default_policy_version: 1,
  })
  const transactions: KernelTransaction[] = []
  let head = genesis.genesis_digest
  for (let stepSeq = 1; stepSeq <= 2; stepSeq += 1) {
    const input = {
      version: 2,
      operation_id: "operation",
      event_id: `opaque-${stepSeq}`,
      event: { kind: `event_${stepSeq}` },
    }
    const step = {
      version: 2,
      operation_id: "operation",
      input_event_id: `opaque-${stepSeq}`,
      step_seq: stepSeq,
      actions: [],
      observations: [{ kind: `accepted_${stepSeq}` }],
      faults: [],
    }
    const transaction = await createKernelTransaction({
      operation_id: "operation",
      step_seq: stepSeq,
      base_generation: stepSeq - 1,
      input,
      step,
      previous_transaction_digest: head,
    })
    transactions.push(transaction)
    head = transaction.transaction_digest
  }
  return { genesis, transactions }
}

describe("rebuildKernelRuntime", () => {
  it("folds only committed inputs and returns the explicit operation cursor", async () => {
    const phases: string[] = []
    const { genesis, transactions } = await fixture()

    const rebuilt = rebuildKernelRuntime(runtime(phases), genesis, transactions)

    expect(phases).toEqual(["prepare:1", "commit:1", "prepare:2", "commit:2"])
    expect(rebuilt.cursor).toEqual({
      operation_id: "operation",
      next_event_sequence: 3,
      next_step_seq: 3,
      transaction_head_digest: transactions[1].transaction_digest,
    })
  })

  it("fails closed and aborts when replayed transition output drifts", async () => {
    const phases: string[] = []
    const { genesis, transactions } = await fixture()

    expect(() => rebuildKernelRuntime(runtime(phases, 2), genesis, transactions)).toThrow(
      KernelLogIntegrityError,
    )
    expect(phases).toEqual(["prepare:1", "commit:1", "prepare:2", "abort:2"])
  })

  it("rejects a runtime whose initial policy differs from genesis", async () => {
    const phases: string[] = []
    const { genesis, transactions } = await fixture()
    const mismatched = runtime(phases)
    const originalSnapshot = mismatched.snapshot
    mismatched.snapshot = () => JSON.stringify({
      ...JSON.parse(originalSnapshot()),
      initial_policy: { ...policy, max_turns: 99 },
    })

    expect(() => rebuildKernelRuntime(mismatched, genesis, transactions)).toThrow(
      "initial policy does not match",
    )
    expect(phases).toEqual([])
  })
})
