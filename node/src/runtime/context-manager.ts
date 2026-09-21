import { createHash } from "node:crypto"

export type ContextItemKind = "core" | "task" | "skill" | "memory" | "knowledge" | "workflow"
export type ContextItemScope = "run" | "session" | "step" | "turn"

export interface ContextItem {
  id: string
  kind: ContextItemKind
  content: string
  scope: ContextItemScope
  priority: number
  tokenCost?: number
  confidence?: number
  expiresAt?: number
  pinned?: boolean
  source: { type: string; id?: string }
}

export interface ContextManagerOptions {
  maxTokens: number
  responseReserveTokens?: number
  now?: () => number
  onEvent?: (event: ContextLedgerEvent) => void
}

export type ContextLedgerEventKind = "context_added" | "context_removed" | "context_expired" | "context_selected"
export interface ContextLedgerEvent {
  kind: ContextLedgerEventKind
  itemId?: string
  source?: ContextItem["source"]
  fingerprint: string
  at: number
}

export interface ContextSnapshot {
  items: ContextItem[]
  selected: ContextItem[]
  usedTokens: number
  availableTokens: number
  fingerprint: string
}

export function estimateContextTokens(content: string): number {
  return Math.max(1, Math.ceil(content.length / 4))
}

/** Host-side context budget and lifecycle manager. Kernel context remains the execution authority. */
export class ContextManager {
  private readonly items = new Map<string, ContextItem>()
  private readonly now: () => number
  private readonly budget: number
  private readonly onEvent?: (event: ContextLedgerEvent) => void

  constructor(options: ContextManagerOptions) {
    if (!Number.isSafeInteger(options.maxTokens) || options.maxTokens < 1) throw new RangeError("maxTokens must be a safe integer >= 1")
    const reserve = options.responseReserveTokens ?? 0
    if (!Number.isSafeInteger(reserve) || reserve < 0 || reserve >= options.maxTokens) throw new RangeError("responseReserveTokens must be >= 0 and lower than maxTokens")
    this.budget = options.maxTokens - reserve
    this.now = options.now ?? Date.now
    this.onEvent = options.onEvent
  }

  upsert(item: ContextItem): void {
    const tokenCost = item.tokenCost ?? estimateContextTokens(item.content)
    if (!Number.isSafeInteger(tokenCost) || tokenCost < 1) throw new RangeError(`context item "${item.id}" has invalid tokenCost`)
    this.items.set(item.id, { ...item, tokenCost })
    this.emit({ kind: "context_added", itemId: item.id, source: item.source })
  }

  remove(id: string): void {
    const item = this.items.get(id)
    if (!item) return
    this.items.delete(id)
    this.emit({ kind: "context_removed", itemId: id, source: item.source })
  }

  expire(now = this.now()): string[] {
    const removed: string[] = []
    for (const [id, item] of this.items) {
      if (item.expiresAt !== undefined && item.expiresAt <= now && !item.pinned) {
        this.items.delete(id)
        removed.push(id)
        this.emit({ kind: "context_expired", itemId: id, source: item.source })
      }
    }
    return removed
  }

  select(now = this.now()): ContextItem[] {
    this.expire(now)
    const selected = this.selectWithoutEvents()
    this.emit({ kind: "context_selected" })
    return selected
  }

  snapshot(now = this.now()): ContextSnapshot {
    this.expire(now)
    const items = [...this.items.values()]
    const selected = this.select(now)
    const usedTokens = selected.reduce((sum, item) => sum + (item.tokenCost ?? estimateContextTokens(item.content)), 0)
    return {
      items,
      selected,
      usedTokens,
      availableTokens: Math.max(0, this.budget - usedTokens),
      fingerprint: this.fingerprint(now),
    }
  }

  fingerprint(now = this.now()): string {
    this.expire(now)
    return this.fingerprintOf(this.selectWithoutEvents())
  }

  private selectWithoutEvents(): ContextItem[] {
    const candidates = [...this.items.values()].sort((left, right) =>
      Number(Boolean(right.pinned)) - Number(Boolean(left.pinned))
      || right.priority - left.priority
      || (right.confidence ?? 0) - (left.confidence ?? 0)
      || left.id.localeCompare(right.id))
    const selected: ContextItem[] = []
    let usedTokens = 0
    for (const item of candidates) {
      const cost = item.tokenCost ?? estimateContextTokens(item.content)
      if (usedTokens + cost > this.budget && !item.pinned) continue
      selected.push(item)
      usedTokens += cost
    }
    return selected
  }

  private emit(event: Omit<ContextLedgerEvent, "fingerprint" | "at">): void {
    try {
      this.onEvent?.({ ...event, fingerprint: this.fingerprintOf(this.selectWithoutEvents()), at: this.now() })
    } catch {
      // Ledger observers are diagnostic only; a faulty sink must never reject context admission.
    }
  }

  private fingerprintOf(selected: ContextItem[]): string {
    const hash = createHash("sha256")
    for (const item of selected) {
      hash.update(JSON.stringify({
        id: item.id,
        kind: item.kind,
        content: item.content,
        scope: item.scope,
        priority: item.priority,
        tokenCost: item.tokenCost ?? estimateContextTokens(item.content),
        confidence: item.confidence ?? null,
        expiresAt: item.expiresAt ?? null,
        pinned: Boolean(item.pinned),
        source: { type: item.source.type, id: item.source.id ?? null },
      }))
      hash.update("\n")
    }
    return `ctx-${hash.digest("hex")}`
  }
}
