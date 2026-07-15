import {
  InMemorySessionLog,
  KernelLogConflictError,
  KernelLogIntegrityError,
  createKernelOperationGenesis,
  createKernelTransaction,
} from "../src/runtime/index.js"

describe("WASM authoritative kernel transaction log", () => {
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
    expect(await log.kernelTransactionHead("session")).toBe(first.transaction_digest)
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
