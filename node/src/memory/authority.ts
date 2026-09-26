import { getKernel } from "../kernel.js"
import type { MemoryRecall, MemoryRecallLifecycle } from "./protocols.js"

/**
 * §22.13 · the kernel's memory authority for a host with no live operation.
 *
 * Inside a run, every memory decision is a kernel transition (`admit_memory_write`, and the
 * `memory_recalled` / `promotion_suggested` facts a query resolution journals). With no run — an
 * explicit `remember` / `recall`, a session extract after the run ended — the host asks the same
 * kernel functions statelessly. The SDK keeps no copy of the validation rule and never computes a
 * recall count itself.
 */

/** The canonical wire `MemoryPolicy` (the `u64` threshold travels as a decimal string). */
export interface KernelMemoryPolicyWire {
  stale_warning_days?: number
  retrieval_top_k?: number
  validation_enabled?: boolean
  max_content_bytes?: number
  max_name_length?: number
  promotion_recall_threshold?: string
}

export interface MemoryPromotion {
  record_id: string
  recall_count: number
}

function ask(request: Record<string, unknown>): Record<string, unknown> {
  return JSON.parse(getKernel().memoryAuthorityJson(JSON.stringify(request))) as Record<string, unknown>
}

/** Would the kernel admit this write? Returns the refusal reason, or `undefined` when admitted. */
export function checkMemoryWrite(
  policy: KernelMemoryPolicyWire | undefined,
  name: string,
  content: string,
): string | undefined {
  const answer = ask({
    op: "check_write",
    ...(policy ? { policy } : {}),
    name,
    content_bytes: Buffer.byteLength(content, "utf8"),
  })
  return answer.admitted === true ? undefined : String(answer.error ?? "memory write refused")
}

/** Derive one recall's lifecycle from the counts the store reported it held. */
export function deriveMemoryRecall(
  policy: KernelMemoryPolicyWire | undefined,
  recalledAt: number,
  hits: MemoryRecall[],
): { recalls: MemoryRecallLifecycle[]; promotions: MemoryPromotion[] } {
  if (hits.length === 0) return { recalls: [], promotions: [] }
  const answer = ask({
    op: "derive_recall",
    ...(policy ? { policy } : {}),
    recalled_at: Math.max(0, Math.trunc(recalledAt)),
    recalls: hits.map(hit => ({
      record_id: hit.record.record_id,
      recall_count: Math.max(0, Math.trunc(hit.record.recall_count ?? 0)),
      ...(hit.record.pinned ? { pinned: true } : {}),
    })),
  })
  return {
    recalls: (answer.recalls ?? []) as MemoryRecallLifecycle[],
    promotions: (answer.promotions ?? []) as MemoryPromotion[],
  }
}
