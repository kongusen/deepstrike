import {
  InMemorySessionLog,
  KernelLogConflictError,
  KernelLogIntegrityError,
  createKernelOperationGenesis,
  createKernelTransaction,
  kernelRecordDigest,
  rebuildKernelRuntime,
  verifyKernelTransactionStream,
} from "../src/runtime/index.js"
import type { KernelRuntimeHandle } from "../src/runtime/kernel-step.js"

const rebuildPolicy = { max_tokens: 8_000, max_turns: 25, max_total_tokens: "0" }
const rebuildDefaults = {
  snapshot_version: 2,
  snapshot_input_limit: 10_000,
  max_input_bytes: 16_777_216,
  snapshot_journal_bytes_limit: 67_108_864,
}

function rebuildRuntime(phases: string[]): KernelRuntimeHandle {
  const step = {
    version: 2,
    operation_id: "op-wasm",
    input_event_id: "opaque",
    step_seq: 1,
    actions: [],
    observations: [],
    faults: [],
  }
  return {
    step: () => "{}",
    prepareStep: input => {
      phases.push("prepare")
      return JSON.stringify({
        status: "prepared",
        base_generation: 0,
        prepare_token: "token",
        input: JSON.parse(input),
        step,
      })
    },
    commitPrepared: () => { phases.push("commit"); return JSON.stringify(step) },
    abortPrepared: () => { phases.push("abort") },
    snapshot: () => JSON.stringify({
      ...rebuildDefaults,
      abi_version: 2,
      initial_policy: rebuildPolicy,
      lifecycle: "created",
      next_step_seq: 1,
      accepted_input_bytes: 0,
      accepted_inputs: [],
    }),
    restore: () => undefined,
    diagnostics: () => "{}",
    isTerminal: () => false,
    turn: () => 0,
    recoveryContentBytes: () => 0,
    render: () => ({ systemText: "", systemStable: "", systemKnowledge: "", turns: [] }),
    drainNewMessages: () => [],
    preservedRefs: () => [],
  }
}

describe("WASM authoritative kernel transaction log", () => {
  it("pins the cross-SDK canonical digest codec", async () => {
    await expect(kernelRecordDigest({ z: 1, a: [true, "雪"] })).resolves.toBe(
      "74ffaa09c9570f87244813a5b15514369f7b1a8996e3e80017585b4df246c1f7",
    )
    await expect(kernelRecordDigest({ ratio: 0.5 })).resolves.toBe(
      "7ae3311a2b33b26525cf688e72ec90df645b018c033d6b9efc23f422af4f8391",
    )
    await expect(kernelRecordDigest({ ratio: Number.POSITIVE_INFINITY })).rejects.toBeInstanceOf(KernelLogIntegrityError)
  })

  it("validates the digest chain and derives a regex-free operation cursor", async () => {
    const genesis = await createKernelOperationGenesis({
      abi_version: 2,
      operation_id: "op-wasm",
      initial_scheduler_policy: { max_tokens: 8_000 },
      resolved_runtime_defaults: {},
      default_policy_version: 1,
    })
    const first = await createKernelTransaction({
      operation_id: "op-wasm",
      step_seq: 1,
      base_generation: 0,
      input: { version: 2, operation_id: "op-wasm", event_id: "opaque-a" },
      step: { version: 2, operation_id: "op-wasm", step_seq: 1, actions: [] },
      previous_transaction_digest: genesis.genesis_digest,
    })

    await expect(verifyKernelTransactionStream(genesis, [first])).resolves.toEqual({
      operation_id: "op-wasm",
      next_event_sequence: 2,
      next_step_seq: 2,
      transaction_head_digest: first.transaction_digest,
    })
  })

  it("deterministically rebuilds a fresh runtime from committed transactions", async () => {
    const phases: string[] = []
    const genesis = await createKernelOperationGenesis({
      abi_version: 2,
      operation_id: "op-wasm",
      initial_scheduler_policy: rebuildPolicy,
      resolved_runtime_defaults: rebuildDefaults,
      default_policy_version: 1,
    })
    const input = { version: 2, operation_id: "op-wasm", event_id: "opaque" }
    const step = {
      version: 2,
      operation_id: "op-wasm",
      input_event_id: "opaque",
      step_seq: 1,
      actions: [],
      observations: [],
      faults: [],
    }
    const transaction = await createKernelTransaction({
      operation_id: "op-wasm",
      step_seq: 1,
      base_generation: 0,
      input,
      step,
      previous_transaction_digest: genesis.genesis_digest,
    })

    const rebuilt = await rebuildKernelRuntime(rebuildRuntime(phases), genesis, [transaction])

    expect(phases).toEqual(["prepare", "commit"])
    expect(rebuilt.cursor.next_event_sequence).toBe(2)
    expect(rebuilt.cursor.transaction_head_digest).toBe(transaction.transaction_digest)
  })

  it("fences stale writers independently of semantic projections", async () => {
    const log = new InMemorySessionLog()
    const genesis = await createKernelOperationGenesis({
      abi_version: 2,
      operation_id: "op-wasm",
      initial_scheduler_policy: { max_tokens: 8_000 },
      resolved_runtime_defaults: { max_input_bytes: 16_777_216 },
      default_policy_version: 1,
    })
    await log.appendKernelGenesis("session", genesis)
    await log.append("session", { kind: "run_started", run_id: "run", goal: "test", criteria: [] })

    const first = await createKernelTransaction({
      operation_id: "op-wasm",
      step_seq: 1,
      base_generation: 0,
      input: { version: 2, operation_id: "op-wasm", event_id: "event-1" },
      step: { version: 2, operation_id: "op-wasm", step_seq: 1, actions: [] },
      previous_transaction_digest: genesis.genesis_digest,
    })
    await log.compareAndAppendKernelTransaction("session", genesis.genesis_digest, first)

    const stale = await createKernelTransaction({
      operation_id: "op-wasm",
      step_seq: 2,
      base_generation: 1,
      input: { version: 2, operation_id: "op-wasm", event_id: "event-2" },
      step: { version: 2, operation_id: "op-wasm", step_seq: 2, actions: [] },
      previous_transaction_digest: genesis.genesis_digest,
    })
    await expect(
      log.compareAndAppendKernelTransaction("session", genesis.genesis_digest, stale),
    ).rejects.toBeInstanceOf(KernelLogConflictError)
    expect(await log.kernelTransactionHead("session", "op-wasm")).toBe(first.transaction_digest)
  })

  it("rejects tampered inputs", async () => {
    const log = new InMemorySessionLog()
    const genesis = await createKernelOperationGenesis({
      abi_version: 2,
      operation_id: "op-wasm",
      initial_scheduler_policy: { max_tokens: 8_000 },
      resolved_runtime_defaults: {},
      default_policy_version: 1,
    })
    await log.appendKernelGenesis("session", genesis)
    const valid = await createKernelTransaction({
      operation_id: "op-wasm",
      step_seq: 1,
      base_generation: 0,
      input: { event_id: "event-1" },
      step: { step_seq: 1 },
      previous_transaction_digest: genesis.genesis_digest,
    })

    await expect(log.compareAndAppendKernelTransaction(
      "session",
      genesis.genesis_digest,
      { ...valid, input: { event_id: "tampered" } },
    )).rejects.toBeInstanceOf(KernelLogIntegrityError)
  })
})
