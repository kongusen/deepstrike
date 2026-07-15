import type { KernelPreparedStep, KernelRuntimeHandle, KernelSnapshotV2 } from "./kernel-step.js"
import {
  KernelLogIntegrityError,
  kernelRecordDigest,
  verifyKernelTransactionStream,
  type KernelOperationCursor,
  type KernelOperationGenesis,
  type KernelTransaction,
} from "./kernel-transaction-log.js"

export interface KernelRebuildResult {
  runtime: KernelRuntimeHandle
  cursor: KernelOperationCursor
}

/** Deterministically fold an authoritative stream into a fresh WASM runtime. */
export async function rebuildKernelRuntime(
  runtime: KernelRuntimeHandle,
  genesis: KernelOperationGenesis,
  transactions: readonly KernelTransaction[],
): Promise<KernelRebuildResult> {
  const cursor = await verifyKernelTransactionStream(genesis, transactions)
  await assertFreshRuntimeMatchesGenesis(runtime, genesis)

  for (const transaction of transactions) {
    const prepared = JSON.parse(runtime.prepareStep(JSON.stringify(transaction.input))) as KernelPreparedStep
    const token = prepared.prepare_token
    try {
      if (prepared.status !== "prepared" || !token) {
        throw new KernelLogIntegrityError(`kernel rebuild step ${transaction.step_seq} was not accepted as a new transition`)
      }
      if (prepared.base_generation !== transaction.base_generation) {
        throw new KernelLogIntegrityError(`kernel rebuild generation diverged at step ${transaction.step_seq}`)
      }
      if ((prepared.step as unknown as { step_seq?: number }).step_seq !== transaction.step_seq) {
        throw new KernelLogIntegrityError(`kernel rebuild step_seq diverged at step ${transaction.step_seq}`)
      }
      if (await kernelRecordDigest(prepared.input) !== transaction.input_digest) {
        throw new KernelLogIntegrityError(`kernel rebuild normalized input diverged at step ${transaction.step_seq}`)
      }
      if (await kernelRecordDigest(prepared.step) !== transaction.step_digest) {
        throw new KernelLogIntegrityError(`kernel rebuild planned step diverged at step ${transaction.step_seq}`)
      }

      const committed = JSON.parse(runtime.commitPrepared(token)) as Record<string, unknown>
      if (await kernelRecordDigest(committed) !== transaction.step_digest) {
        throw new KernelLogIntegrityError(`kernel rebuild committed step diverged at step ${transaction.step_seq}`)
      }
    } catch (error) {
      if (token && prepared.status === "prepared") {
        try { runtime.abortPrepared(token) } catch { /* discard failed runtime */ }
      }
      throw error
    }
  }

  return { runtime, cursor }
}

async function assertFreshRuntimeMatchesGenesis(
  runtime: KernelRuntimeHandle,
  genesis: KernelOperationGenesis,
): Promise<void> {
  const snapshot = JSON.parse(runtime.snapshot()) as KernelSnapshotV2
  if (snapshot.abi_version !== genesis.abi_version) {
    throw new KernelLogIntegrityError("kernel runtime ABI version does not match operation genesis")
  }
  if (genesis.default_policy_version !== 1) {
    throw new KernelLogIntegrityError("kernel operation uses an unsupported default policy version")
  }
  if (snapshot.operation_id || snapshot.next_step_seq !== 1 || snapshot.accepted_inputs.length !== 0) {
    throw new KernelLogIntegrityError("kernel rebuild requires a fresh runtime")
  }
  if (await kernelRecordDigest(snapshot.initial_policy) !== await kernelRecordDigest(genesis.initial_scheduler_policy)) {
    throw new KernelLogIntegrityError("kernel runtime initial policy does not match operation genesis")
  }
  const defaults = {
    snapshot_version: snapshot.snapshot_version,
    snapshot_input_limit: snapshot.snapshot_input_limit,
    max_input_bytes: snapshot.max_input_bytes,
    snapshot_journal_bytes_limit: snapshot.snapshot_journal_bytes_limit,
  }
  if (await kernelRecordDigest(defaults) !== await kernelRecordDigest(genesis.resolved_runtime_defaults)) {
    throw new KernelLogIntegrityError("kernel runtime defaults do not match operation genesis")
  }
}
