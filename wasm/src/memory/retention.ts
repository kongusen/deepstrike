/**
 * Host-side mirror of the kernel's shared retention vocabulary (`crates/.../mm/value.rs` +
 * `memory_retention_score`). The durable store owns the full cross-session record set, so it — not
 * the kernel — enforces the capacity bound; it must rank by the same integer "value" definition so
 * memory and context knowledge agree on what is worth keeping.
 *
 * Integer terms (magnitudes well within 2^53, so JS number math is exact). The turn-based recency
 * term is omitted here: the host store has no turn counter, so a record's recall_count, kind,
 * confidence, size, and (clock-based) staleness drive the ordering. `parity` tests pin this against
 * the Rust reference for the terms both compute.
 */
import type { MemoryRecord, MemoryKind } from "./index.js"

const DAY_MS = 86_400_000

const KIND_WEIGHT: Record<MemoryKind, number> = {
  user: 1_600,
  feedback: 1_800,
  project: 1_400,
  reference: 1_200,
}

/** floor(log2(1 + n)) — the kernel's stable ln(1+n) proxy for usage. */
function usageBucket(useCount: number): number {
  if (useCount <= 0) return 0
  return Math.floor(Math.log2(useCount + 1))
}

/** Day-based staleness discount in parts-per-million, matching the store's recall-ranking penalty
 *  band but expressed on the kernel's ppm scale. */
function staleDiscountPpm(
  updatedAt: number,
  ttlDays: number | undefined,
  nowMs: number,
  staleWarningDays: number,
): number {
  const ageDays = Math.max(0, (nowMs - updatedAt) / DAY_MS)
  let ppm = 0
  if (ageDays > staleWarningDays) {
    ppm += Math.min(300_000, 50_000 * (ageDays - staleWarningDays))
  }
  if (ttlDays !== undefined && ageDays > ttlDays) {
    ppm += 400_000
  }
  return Math.min(1_000_000, Math.floor(ppm))
}

/**
 * Deterministic retention score for a durable record. Higher is retained first;
 * `Number.POSITIVE_INFINITY` is reserved for pins (the kernel uses `i64::MAX`).
 */
export function memoryRetentionScore(
  record: MemoryRecord,
  nowMs: number,
  staleWarningDays: number,
): number {
  if (record.pinned) return Number.POSITIVE_INFINITY

  const usage = usageBucket(record.recall_count) * 8_192
  // Recency (turn-based in the kernel) is unavailable host-side; omitted deterministically.
  const kind = KIND_WEIGHT[record.kind] ?? 0
  const confidencePpm = Math.min(1_000_000, Math.floor(Math.max(0, Math.min(1, record.confidence)) * 1_000_000))
  const confidence = Math.floor(confidencePpm / 250)
  const stalePpm = staleDiscountPpm(record.updated_at, record.ttl_days, nowMs, staleWarningDays)
  const staleness = Math.floor(stalePpm / 125)
  // 4-bytes-per-token proxy, matching the kernel's content.len()/4.
  const tokens = Math.min(0xffffffff, Math.floor(byteLength(record.content) / 4))
  const size = tokens * 4

  return usage + kind + confidence - staleness - size
}

function byteLength(text: string): number {
  // Match Rust's content.len() (UTF-8 bytes) without pulling in Buffer for portability.
  let bytes = 0
  for (const ch of text) {
    const code = ch.codePointAt(0)!
    bytes += code <= 0x7f ? 1 : code <= 0x7ff ? 2 : code <= 0xffff ? 3 : 4
  }
  return bytes
}
