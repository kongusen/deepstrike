export type CanonicalStopReason =
  | "end_turn"
  | "tool_use"
  | "max_tokens"
  | "stop_sequence"
  | "content_filter"
  | "other"

const KNOWN = new Set<CanonicalStopReason>([
  "end_turn",
  "tool_use",
  "max_tokens",
  "stop_sequence",
  "content_filter",
  "other",
])

/** Validates the closed, provider-normalized stop-reason contract. */
export function decodeCanonicalStopReason(value: unknown): CanonicalStopReason {
  if (typeof value !== "string" || !KNOWN.has(value as CanonicalStopReason)) {
    throw new RangeError(`unknown canonical stop reason: ${String(value)}`)
  }
  return value as CanonicalStopReason
}

/** Normalize an open-ended provider value into the closed runtime contract. */
export function normalizeProviderStopReason(value: unknown): CanonicalStopReason {
  if (typeof value !== "string" || value.length === 0) {
    throw new RangeError(`invalid provider stop reason: ${String(value)}`)
  }
  return KNOWN.has(value as CanonicalStopReason)
    ? value as CanonicalStopReason
    : "other"
}
