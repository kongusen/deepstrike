import { createHash } from "node:crypto"

export const KERNEL_LOG_RECORD_VERSION = 1 as const

export interface KernelOperationGenesisBody {
  record_version: typeof KERNEL_LOG_RECORD_VERSION
  abi_version: number
  operation_id: string
  initial_scheduler_policy: Record<string, unknown>
  resolved_runtime_defaults: Record<string, unknown>
  default_policy_version: number
}

export interface KernelOperationGenesis extends KernelOperationGenesisBody {
  genesis_digest: string
}

export interface KernelTransactionBody {
  record_version: typeof KERNEL_LOG_RECORD_VERSION
  operation_id: string
  step_seq: number
  base_generation: number
  input: Record<string, unknown>
  input_digest: string
  previous_transaction_digest: string
  step_digest: string
}

export interface KernelTransaction extends KernelTransactionBody {
  transaction_digest: string
}

export interface KernelGenesisReceipt {
  log_seq: number
  genesis_digest: string
}

export interface DurableAppendReceipt {
  log_seq: number
  transaction_digest: string
}

export class KernelLogConflictError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "KernelLogConflictError"
  }
}

export class KernelLogIntegrityError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "KernelLogIntegrityError"
  }
}

export function canonicalKernelJson(value: unknown): string {
  if (value === null) return "null"
  if (typeof value === "string" || typeof value === "boolean") return JSON.stringify(value)
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) {
      throw new KernelLogIntegrityError(
        "canonical records require safe integers; encode ratios as fixed-point integers or decimal strings",
      )
    }
    return JSON.stringify(Object.is(value, -0) ? 0 : value)
  }
  if (Array.isArray(value)) return `[${value.map(canonicalKernelJson).join(",")}]`
  if (typeof value === "object") {
    const object = value as Record<string, unknown>
    const keys = Object.keys(object).sort()
    return `{${keys.map(key => `${JSON.stringify(key)}:${canonicalKernelJson(object[key])}`).join(",")}}`
  }
  throw new KernelLogIntegrityError(`unsupported canonical record value: ${typeof value}`)
}

export function kernelRecordDigest(value: unknown): string {
  return createHash("sha256").update(canonicalKernelJson(value), "utf8").digest("hex")
}

export async function createKernelOperationGenesis(
  input: Omit<KernelOperationGenesisBody, "record_version">,
): Promise<KernelOperationGenesis> {
  const body: KernelOperationGenesisBody = {
    record_version: KERNEL_LOG_RECORD_VERSION,
    ...input,
  }
  validateGenesisBody(body)
  return { ...body, genesis_digest: kernelRecordDigest(body) }
}

export async function createKernelTransaction(input: {
  operation_id: string
  step_seq: number
  base_generation: number
  input: Record<string, unknown>
  step: Record<string, unknown>
  previous_transaction_digest: string
}): Promise<KernelTransaction> {
  const body: KernelTransactionBody = {
    record_version: KERNEL_LOG_RECORD_VERSION,
    operation_id: input.operation_id,
    step_seq: input.step_seq,
    base_generation: input.base_generation,
    input: input.input,
    input_digest: kernelRecordDigest(input.input),
    previous_transaction_digest: input.previous_transaction_digest,
    step_digest: kernelRecordDigest(input.step),
  }
  validateTransactionBody(body)
  return { ...body, transaction_digest: kernelRecordDigest(body) }
}

export function verifyKernelOperationGenesis(genesis: KernelOperationGenesis): void {
  const { genesis_digest, ...body } = genesis
  validateGenesisBody(body)
  if (kernelRecordDigest(body) !== genesis_digest) {
    throw new KernelLogIntegrityError("kernel genesis digest does not match its canonical body")
  }
}

export function verifyKernelTransaction(transaction: KernelTransaction): void {
  const { transaction_digest, ...body } = transaction
  validateTransactionBody(body)
  if (kernelRecordDigest(body.input) !== body.input_digest) {
    throw new KernelLogIntegrityError("kernel transaction input digest does not match its input")
  }
  if (kernelRecordDigest(body) !== transaction_digest) {
    throw new KernelLogIntegrityError("kernel transaction digest does not match its canonical body")
  }
}

function validateGenesisBody(genesis: KernelOperationGenesisBody): void {
  if (genesis.record_version !== KERNEL_LOG_RECORD_VERSION) {
    throw new KernelLogIntegrityError("unsupported kernel genesis record version")
  }
  if (!Number.isSafeInteger(genesis.abi_version) || genesis.abi_version <= 0) {
    throw new KernelLogIntegrityError("kernel genesis abi_version must be a positive safe integer")
  }
  if (!genesis.operation_id) throw new KernelLogIntegrityError("kernel genesis operation_id is required")
  if (!Number.isSafeInteger(genesis.default_policy_version) || genesis.default_policy_version <= 0) {
    throw new KernelLogIntegrityError("kernel genesis default_policy_version must be a positive safe integer")
  }
}

function validateTransactionBody(transaction: KernelTransactionBody): void {
  if (transaction.record_version !== KERNEL_LOG_RECORD_VERSION) {
    throw new KernelLogIntegrityError("unsupported kernel transaction record version")
  }
  if (!transaction.operation_id) throw new KernelLogIntegrityError("kernel transaction operation_id is required")
  if (!Number.isSafeInteger(transaction.step_seq) || transaction.step_seq <= 0) {
    throw new KernelLogIntegrityError("kernel transaction step_seq must be a positive safe integer")
  }
  if (!Number.isSafeInteger(transaction.base_generation) || transaction.base_generation < 0) {
    throw new KernelLogIntegrityError("kernel transaction base_generation must be a non-negative safe integer")
  }
  if (!transaction.previous_transaction_digest) {
    throw new KernelLogIntegrityError("kernel transaction previous_transaction_digest is required")
  }
}
