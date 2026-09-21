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
}

export interface ContextSnapshot {
  items: ContextItem[]
  selected: ContextItem[]
  usedTokens: number
  availableTokens: number
}

export function estimateContextTokens(content: string): number {
  return Math.max(1, Math.ceil(content.length / 4))
}

/** Host-side context budget and lifecycle manager. Kernel context remains the execution authority. */
export class ContextManager {
  private readonly items = new Map<string, ContextItem>()
  private readonly now: () => number
  private readonly budget: number

  constructor(options: ContextManagerOptions) {
    if (!Number.isSafeInteger(options.maxTokens) || options.maxTokens < 1) throw new RangeError("maxTokens must be a safe integer >= 1")
    const reserve = options.responseReserveTokens ?? 0
    if (!Number.isSafeInteger(reserve) || reserve < 0 || reserve >= options.maxTokens) throw new RangeError("responseReserveTokens must be >= 0 and lower than maxTokens")
    this.budget = options.maxTokens - reserve
    this.now = options.now ?? Date.now
  }

  upsert(item: ContextItem): void {
    const tokenCost = item.tokenCost ?? estimateContextTokens(item.content)
    if (!Number.isSafeInteger(tokenCost) || tokenCost < 1) throw new RangeError(`context item "${item.id}" has invalid tokenCost`)
    this.items.set(item.id, { ...item, tokenCost })
  }

  remove(id: string): void { this.items.delete(id) }

  expire(now = this.now()): string[] {
    const removed: string[] = []
    for (const [id, item] of this.items) {
      if (item.expiresAt !== undefined && item.expiresAt <= now && !item.pinned) {
        this.items.delete(id)
        removed.push(id)
      }
    }
    return removed
  }

  select(now = this.now()): ContextItem[] {
    this.expire(now)
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

  snapshot(now = this.now()): ContextSnapshot {
    const items = [...this.items.values()]
    const selected = this.select(now)
    const usedTokens = selected.reduce((sum, item) => sum + (item.tokenCost ?? estimateContextTokens(item.content)), 0)
    return { items, selected, usedTokens, availableTokens: Math.max(0, this.budget - usedTokens) }
  }
}
