import { getKernel } from "../runtime/kernel.js"
import type { MemoryRecall, MemoryRecallLifecycle } from "./index.js"

/**
 * §22.13 · the kernel's memory authority for a host with no live operation.
 *
 * Inside a run, every memory decision is a kernel transition (`admit_memory_write`, and the
 * `memory_recalled` / `promotion_suggested` facts a query resolution journals). With no run — an
 * explicit `remember`, a session extract after the run ended — the host asks the same kernel
 * functions statelessly. The SDK keeps no copy of the validation rule and never computes a recall
 * count itself.
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

export interface MemoryPromotion { record_id: string; recall_count: number }

async function ask(request: Record<string, unknown>): Promise<Record<string, unknown>> {
  const kernel = await getKernel()
  return JSON.parse(kernel.memoryAuthorityJson(JSON.stringify(request))) as Record<string, unknown>
}

/** Would the kernel admit this write? Resolves to the refusal reason, or `undefined` when admitted. */
export async function checkMemoryWrite(
  policy: KernelMemoryPolicyWire | undefined,
  name: string,
  content: string,
): Promise<string | undefined> {
  const answer = await ask({
    op: "check_write",
    ...(policy ? { policy } : {}),
    name,
    content_bytes: new TextEncoder().encode(content).byteLength,
  })
  return answer.admitted === true ? undefined : String(answer.error ?? "memory write refused")
}

/** Derive one recall's lifecycle from the counts the store reported it held. */
export async function deriveMemoryRecall(
  policy: KernelMemoryPolicyWire | undefined,
  recalledAt: number,
  hits: MemoryRecall[],
): Promise<{ recalls: MemoryRecallLifecycle[]; promotions: MemoryPromotion[] }> {
  if (hits.length === 0) return { recalls: [], promotions: [] }
  const answer = await ask({
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
