export interface ReactionRecord {
  personaId: string
  output: string
}

export interface ReactionCheckpointReceipt {
  checkpointKey: string
  leaseToken: string
}

export interface ReactionCheckpointClaim extends ReactionCheckpointReceipt {
  leaseExpiresAtMs: number
  plan?: string[]
  outputs: Record<string, string>
}

export type ReactionCheckpointClaimResult =
  | { status: "claimed"; claim: ReactionCheckpointClaim }
  | { status: "completed"; reactions: ReactionRecord[] }
  | { status: "busy" }

export interface ReactionCheckpointStore {
  claim(checkpointKey: string, leaseMs?: number): Promise<ReactionCheckpointClaimResult>
  savePlan(receipt: ReactionCheckpointReceipt, personaIds: string[]): Promise<string[] | null>
  record(receipt: ReactionCheckpointReceipt, reaction: ReactionRecord): Promise<boolean>
  complete(receipt: ReactionCheckpointReceipt): Promise<boolean>
  release(receipt: ReactionCheckpointReceipt): Promise<boolean>
}

interface CheckpointState {
  plan?: string[]
  outputs: Map<string, string>
  completed: boolean
  lease?: { token: string; expiresAtMs: number }
}

export interface InMemoryReactionCheckpointStoreOptions {
  now?: () => number
  defaultLeaseMs?: number
}

/** Process-local reference implementation; durable stores implement the same atomic contract. */
export class InMemoryReactionCheckpointStore implements ReactionCheckpointStore {
  private readonly states = new Map<string, CheckpointState>()
  private leaseSeq = 0

  constructor(private readonly opts: InMemoryReactionCheckpointStoreOptions = {}) {}

  async claim(checkpointKey: string, leaseMs = this.opts.defaultLeaseMs ?? 900_000): Promise<ReactionCheckpointClaimResult> {
    if (!Number.isFinite(leaseMs) || leaseMs <= 0) throw new RangeError("leaseMs must be positive")
    const now = this.opts.now?.() ?? Date.now()
    const state: CheckpointState = this.states.get(checkpointKey) ?? {
      outputs: new Map<string, string>(),
      completed: false,
    }
    this.states.set(checkpointKey, state)
    if (state.completed) return { status: "completed", reactions: this.reactions(state) }
    if (state.lease && state.lease.expiresAtMs > now) return { status: "busy" }
    const token = `${checkpointKey}:lease-${++this.leaseSeq}`
    const expiresAtMs = now + leaseMs
    state.lease = { token, expiresAtMs }
    return {
      status: "claimed",
      claim: {
        checkpointKey,
        leaseToken: token,
        leaseExpiresAtMs: expiresAtMs,
        plan: state.plan ? [...state.plan] : undefined,
        outputs: Object.fromEntries(state.outputs),
      },
    }
  }

  async savePlan(receipt: ReactionCheckpointReceipt, personaIds: string[]): Promise<string[] | null> {
    const state = this.current(receipt)
    if (!state) return null
    state.plan ??= [...new Set(personaIds)]
    return [...state.plan]
  }

  async record(receipt: ReactionCheckpointReceipt, reaction: ReactionRecord): Promise<boolean> {
    const state = this.current(receipt)
    if (!state) return false
    state.outputs.set(reaction.personaId, reaction.output)
    return true
  }

  async complete(receipt: ReactionCheckpointReceipt): Promise<boolean> {
    const state = this.current(receipt)
    if (!state) return false
    if (!state.plan || state.plan.some(personaId => !state.outputs.has(personaId))) {
      throw new Error("cannot complete a reaction checkpoint with unfinished personas")
    }
    state.completed = true
    delete state.lease
    return true
  }

  async release(receipt: ReactionCheckpointReceipt): Promise<boolean> {
    const state = this.current(receipt)
    if (!state) return false
    delete state.lease
    return true
  }

  private current(receipt: ReactionCheckpointReceipt): CheckpointState | undefined {
    const state = this.states.get(receipt.checkpointKey)
    return state?.lease?.token === receipt.leaseToken ? state : undefined
  }

  private reactions(state: CheckpointState): ReactionRecord[] {
    return (state.plan ?? []).map(personaId => ({ personaId, output: state.outputs.get(personaId)! }))
  }
}

export class ReactionInProgressError extends Error {
  constructor(readonly checkpointKey: string) {
    super(`reaction checkpoint is already in progress: ${checkpointKey}`)
    this.name = "ReactionInProgressError"
  }
}
