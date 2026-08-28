import type { CanonicalKernelInstance } from "../kernel.js"
import type { KernelJournal } from "./kernel-journal.js"

/**
 * What a journal inspection knows: whether the operation restores, and — when it does not — which
 * record is the first this binary cannot replay. The effect manifest (which effects each step
 * published) lives in the host event log via the `step_published_effects` observation, not here:
 * a journal stores only each record's input and digests, never the step it produced (§8.1).
 */
export interface KernelJournalDiagnosis {
  operationId: string
  recordCount: number
  /** True when `restore()` replayed the whole journal and verified every step digest. */
  restorable: boolean
  /**
   * The `step_seq` of the first record whose replay diverged from the durable digest, when known.
   * The restore failure message names this step; a fault before any record replays leaves it null.
   */
  divergenceStep: number | null
  divergenceReason: string | null
  /** The operation's terminal, only when the journal restored cleanly. */
  terminal: Record<string, unknown> | undefined
  /** Pending effects at the head, only when the journal restored cleanly. */
  pendingEffects: Array<{ effect_id: string; kind: string }>
}

const DIVERGENCE_STEP = /at step (\d+)/

/**
 * Inspect a durable journal without committing anything: replay it through the kernel and report
 * whether it still restores. This turns a "silent brick" — an operation whose journal was appended
 * but whose step can no longer be re-derived by this binary — into an actionable diagnosis: which
 * record diverges, and what the head still holds if it does not.
 *
 * Nothing is mutated; the same journal can be diagnosed repeatedly and then restored normally.
 */
export async function diagnoseKernelJournal(
  kernel: CanonicalKernelInstance,
  journal: KernelJournal,
  operationId: string,
): Promise<KernelJournalDiagnosis> {
  const checkpoint = await journal.latestCheckpoint(operationId)
  const records = await journal.recordsAfter(operationId, checkpoint?.covered_head)

  try {
    const cost = kernel.restore(
      checkpoint ? Buffer.from(checkpoint.checkpoint_bytes) : undefined,
      records.map(record => Buffer.from(record.record_bytes)),
    )
    return {
      operationId,
      recordCount: records.length,
      restorable: true,
      divergenceStep: null,
      divergenceReason: null,
      terminal: parseJson(kernel.terminalJson()) as Record<string, unknown> | undefined,
      pendingEffects: parsePendingEffects(kernel.pendingEffectsJson()),
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    const match = DIVERGENCE_STEP.exec(message)
    return {
      operationId,
      recordCount: records.length,
      restorable: false,
      divergenceStep: match ? Number(match[1]) : null,
      divergenceReason: message,
      terminal: undefined,
      pendingEffects: [],
    }
  }
}

function parseJson(value: string | null | undefined): unknown {
  if (!value) return undefined
  try {
    return JSON.parse(value)
  } catch {
    return undefined
  }
}

function parsePendingEffects(raw: string): Array<{ effect_id: string; kind: string }> {
  const parsed = parseJson(raw)
  if (!Array.isArray(parsed)) return []
  return parsed.map(envelope => {
    const effect = (envelope as Record<string, unknown>)?.effect as Record<string, unknown> | undefined
    return {
      effect_id: String((envelope as Record<string, unknown>)?.effect_id ?? ""),
      kind: String(effect?.kind ?? ""),
    }
  })
}
