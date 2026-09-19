import { getKernel } from "../kernel.js"

/** Framework-facing API plus the Rust verifiable-runtime report ABI. */
export const VERIFIABLE_REPORT_SCHEMA = "verifiable-report/v2" as const
export const VERIFIABLE_FORK_SCHEMA = "verifiable-fork/v2" as const

export type VerifiableCommand = "inspect" | "verify" | "replay" | "fork"
export type CheckVerdict = "pass" | "degraded" | "fail" | "unavailable"
export type ReplayVerdict = "pass" | "fail" | "unavailable"
export type VerifiableReport = {
  readonly schema: typeof VERIFIABLE_REPORT_SCHEMA
  readonly command: VerifiableCommand
  readonly operation_id: string
  readonly [key: string]: unknown
}

export interface VerifiableEvidence {
  readonly journal: ReadonlyArray<Uint8Array>
  readonly sessionLogs?: ReadonlyArray<ReadonlyArray<Uint8Array>>
  readonly checkpoints?: ReadonlyArray<Uint8Array>
}

export interface VerifyOptions {
  readonly strict?: boolean
  readonly requireComplete?: boolean
}

export interface ReplayOptions {
  readonly strict?: boolean
  readonly atStep?: number
}

export interface ForkPlan extends VerifiableForkManifest {
  readonly at_step: string
}

export interface VerifiableRuntimeAdapter {
  inspect(operationId: string, evidence: VerifiableEvidence, strict: boolean): VerifiableReport
  verify(operationId: string, evidence: VerifiableEvidence, options: VerifyOptions): VerifiableReport
  replay(operationId: string, evidence: VerifiableEvidence, options: ReplayOptions): VerifiableReport
  prepareFork(
    operationId: string,
    evidence: VerifiableEvidence,
    atStep: number,
    strict: boolean,
  ): ForkPlan
}

export type VerifiableOperationJson = (request: string) => string

/** Create the SDK adapter backed by the Rust core JSON bridge. */
export function createVerifiableRuntimeAdapter(
  operationJson: VerifiableOperationJson,
): VerifiableRuntimeAdapter {
  const call = (
    operationId: string,
    evidence: VerifiableEvidence,
    command: VerifiableCommand,
    options: VerifyOptions | (ReplayOptions & { requireComplete?: boolean }) = {},
  ): VerifiableReport | ForkPlan => {
    const result = JSON.parse(operationJson(JSON.stringify({
      operation_id: operationId,
      command,
      evidence: {
        journal: evidence.journal.map((bytes) => Array.from(bytes)),
        session_logs: (evidence.sessionLogs ?? []).map((stream) => stream.map((bytes) => Array.from(bytes))),
        checkpoints: (evidence.checkpoints ?? []).map((bytes) => Array.from(bytes)),
      },
      strict: options.strict ?? false,
      require_complete: "requireComplete" in options ? options.requireComplete ?? false : false,
      at_step: "atStep" in options ? options.atStep : undefined,
    }))) as { schema?: unknown }
    if (command === "fork") return result as ForkPlan
    assertVerifiableReportSchema(result)
    return result as VerifiableReport
  }
  return {
    inspect: (operationId, evidence, strict) => call(operationId, evidence, "inspect", { strict }) as VerifiableReport,
    verify: (operationId, evidence, options) => call(operationId, evidence, "verify", options) as VerifiableReport,
    replay: (operationId, evidence, options) => call(operationId, evidence, "replay", options) as VerifiableReport,
    prepareFork: (operationId, evidence, atStep, strict) => call(operationId, evidence, "fork", { atStep, strict }) as ForkPlan,
  }
}

/** Create the adapter from the installed native Node binding. */
export function createNativeVerifiableRuntimeAdapter(): VerifiableRuntimeAdapter {
  return createVerifiableRuntimeAdapter(getKernel().verifiableOperationJson)
}

/**
 * Storage-neutral framework handle. The adapter is the single semantic bridge to Rust core;
 * filesystem, database, and browser stores implement evidence loading outside this class.
 */
export class VerifiableOperation {
  readonly operationId: string
  readonly evidence: VerifiableEvidence
  private readonly adapter: VerifiableRuntimeAdapter

  constructor(operationId: string, evidence: VerifiableEvidence, adapter: VerifiableRuntimeAdapter) {
    this.operationId = operationId
    this.evidence = evidence
    this.adapter = adapter
  }

  inspect(strict = false): VerifiableReport {
    return this.adapter.inspect(this.operationId, this.evidence, strict)
  }

  verify(options: VerifyOptions = {}): VerifiableReport {
    return this.adapter.verify(this.operationId, this.evidence, options)
  }

  replay(options: ReplayOptions = {}): VerifiableReport {
    return this.adapter.replay(this.operationId, this.evidence, options)
  }

  prepareFork(atStep: number, strict = false): ForkPlan {
    return this.adapter.prepareFork(this.operationId, this.evidence, atStep, strict)
  }
}

export interface VerifiableForkManifest {
  schema: typeof VERIFIABLE_FORK_SCHEMA
  operation_id: string
  at_step: string
  parent_record_digest: string
  parent_input_id: string
  source_records: number
}

export function assertVerifiableReportSchema(value: { schema?: unknown }): void {
  if (value.schema !== VERIFIABLE_REPORT_SCHEMA) {
    throw new Error(`unsupported verifiable report schema: ${String(value.schema)}`)
  }
}
