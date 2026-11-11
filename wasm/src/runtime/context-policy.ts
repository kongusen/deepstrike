export const PPM_SCALE = 1_000_000 as const

export interface ContextPressureThresholds {
  snip: number
  micro: number
  collapse: number
  auto: number
  renewal: number
}

export interface ContextPolicy {
  pressureThresholds: ContextPressureThresholds
  targetAfterCompress: number
  preserveRecentTurns: number
  renewalCarryover: number
  collapseOldAssistantNarration: boolean
  idleMicroCompactMinutes: number
  /** Plan task 9.1 · tokens the model's notes may hold in the state turn; 0 (the default) turns the
   *  `note` tool off. Notes are written by the model and kept verbatim through compaction. */
  notesBudgetTokens?: number
  /** Plan task 9.2 · range compaction pages out results a note cites first, shows archived
   *  citations as `read_result` handles, and its summary only locates the archive. Off by default. */
  compactionReadsNotes?: boolean
  /** Plan task 9.2 · a range compaction about to archive results no note cites waits one turn,
   *  listing them in the task state. Off by default. */
  pageOutNotice?: boolean
  /** Plan task 9.3 · structural diagnostics of the notes (dead links, re-read evidence, near
   *  duplicates, long-open questions) in the task state, for the model to act on. Off by default. */
  noteDiagnostics?: boolean
}

export interface ContextPolicyWire {
  pressure_thresholds_ppm: ContextPressureThresholds
  target_after_compress_ppm: number
  preserve_recent_turns: number
  renewal_carryover_ppm: number
  collapse_old_assistant_narration: boolean
  idle_micro_compact_minutes: number
  notes_budget_tokens?: number
  compaction_reads_notes?: boolean
  page_out_notice?: boolean
  note_diagnostics?: boolean
}

export interface ContextPolicyOverrides extends Partial<Omit<ContextPolicy, "pressureThresholds">> {
  pressureThresholds?: Partial<ContextPressureThresholds>
}

export const DEFAULT_CONTEXT_POLICY: Readonly<ContextPolicy> = Object.freeze({
  pressureThresholds: Object.freeze({ snip: 0.70, micro: 0.80, collapse: 0.90, auto: 0.95, renewal: 0.98 }),
  targetAfterCompress: 0.65,
  preserveRecentTurns: 2,
  renewalCarryover: 0.05,
  collapseOldAssistantNarration: true,
  idleMicroCompactMinutes: 60,
})

/** Resolve ergonomic partial SDK options into one complete, atomically validated policy. */
export function contextPolicy(overrides: ContextPolicyOverrides = {}): ContextPolicy {
  const policy: ContextPolicy = {
    ...DEFAULT_CONTEXT_POLICY,
    ...overrides,
    pressureThresholds: {
      ...DEFAULT_CONTEXT_POLICY.pressureThresholds,
      ...overrides.pressureThresholds,
    },
  }
  normalizeContextPolicy(policy)
  return policy
}

/** Convert the public ratio-based policy to the canonical integer-only ABI wire shape. */
export function normalizeContextPolicy(policy: ContextPolicy): ContextPolicyWire {
  const pressure_thresholds_ppm = {
    snip: ratioToPpm(policy.pressureThresholds.snip, "pressureThresholds.snip"),
    micro: ratioToPpm(policy.pressureThresholds.micro, "pressureThresholds.micro"),
    collapse: ratioToPpm(policy.pressureThresholds.collapse, "pressureThresholds.collapse"),
    auto: ratioToPpm(policy.pressureThresholds.auto, "pressureThresholds.auto"),
    renewal: ratioToPpm(policy.pressureThresholds.renewal, "pressureThresholds.renewal"),
  }
  const ordered = Object.values(pressure_thresholds_ppm)
  if (!ordered.every((value, index) => index === 0 || ordered[index - 1] < value)) {
    throw new RangeError("context pressure thresholds must satisfy snip < micro < collapse < auto < renewal")
  }
  const target_after_compress_ppm = ratioToPpm(policy.targetAfterCompress, "targetAfterCompress")
  if (target_after_compress_ppm >= pressure_thresholds_ppm.snip) {
    throw new RangeError("targetAfterCompress must be lower than the snip threshold")
  }
  assertIntegerAtLeast(policy.preserveRecentTurns, 1, "preserveRecentTurns")
  assertIntegerAtLeast(policy.idleMicroCompactMinutes, 0, "idleMicroCompactMinutes")
  if (typeof policy.collapseOldAssistantNarration !== "boolean") {
    throw new TypeError("collapseOldAssistantNarration must be boolean")
  }
  const notes = policy.notesBudgetTokens ?? 0
  assertIntegerAtLeast(notes, 0, "notesBudgetTokens")
  for (const field of ["compactionReadsNotes", "pageOutNotice", "noteDiagnostics"] as const) {
    if (policy[field] !== undefined && typeof policy[field] !== "boolean") throw new TypeError(`${field} must be boolean`)
  }

  return {
    pressure_thresholds_ppm,
    target_after_compress_ppm,
    preserve_recent_turns: policy.preserveRecentTurns,
    renewal_carryover_ppm: ratioToPpm(policy.renewalCarryover, "renewalCarryover"),
    collapse_old_assistant_narration: policy.collapseOldAssistantNarration,
    idle_micro_compact_minutes: policy.idleMicroCompactMinutes,
    ...(notes > 0 ? { notes_budget_tokens: notes } : {}),
    ...(policy.compactionReadsNotes ? { compaction_reads_notes: true } : {}),
    ...(policy.pageOutNotice ? { page_out_notice: true } : {}),
    ...(policy.noteDiagnostics ? { note_diagnostics: true } : {}),
  }
}

export function ratioToPpm(value: number, field = "ratio"): number {
  if (!Number.isFinite(value) || value < 0 || value > 1) {
    throw new RangeError(`${field} must be a finite number between 0 and 1`)
  }
  return Math.floor(value * PPM_SCALE + 0.5)
}

function assertIntegerAtLeast(value: number, minimum: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < minimum) {
    throw new RangeError(`${field} must be a safe integer >= ${minimum}`)
  }
}
