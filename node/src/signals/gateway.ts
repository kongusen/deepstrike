import type {
  LeasedSignalSource,
  RuntimeSignal,
  SignalClaim,
  SignalDeliveryReceipt,
} from "./types.js"
import type { ScheduledPrompt } from "./scheduled.js"
import type { ObserverErrorHandler } from "../runtime/reliability.js"
import { reportObserverFailure } from "../runtime/reliability.js"

export interface SignalGatewayOptions {
  onObserverError?: ObserverErrorHandler
  /** Injectable wall clock for deterministic lease tests. */
  now?: () => number
  /** Default claim lease. Must be positive. Default: 30 seconds. */
  defaultLeaseMs?: number
}

interface QueuedSignal {
  deliveryId: string
  signal: RuntimeSignal
  lease?: { token: string; expiresAtMs: number }
}

/**
 * SignalGateway — entry point for all external signals into the agent.
 *
 * Implements `SignalSource` so it can be passed directly to `AgentOptions.signalSource`.
 * The gateway maintains an internal FIFO queue; `nextSignal()` drains it one entry at a time
 * on each agent turn.
 *
 * Responsibilities:
 * - Cron scheduling: fires ScheduledPrompts at the right wall-clock time (idempotent by goal+time)
 * - Webhook / push ingestion: external code calls `ingest()` to push a signal in
 * - Listener API: `onSignal()` for side-channel observers that don't need the pull interface
 */
export class SignalGateway implements LeasedSignalSource {
  private timers = new Map<string, ReturnType<typeof setTimeout>>()
  private queue: QueuedSignal[] = []
  private listeners: Array<(sig: RuntimeSignal) => void> = []
  private deliverySeq = 0
  private leaseSeq = 0

  constructor(private readonly opts: SignalGatewayOptions = {}) {}

  // ── SignalSource interface (pull model) ─────────────────────────────────────

  /**
   * Called by the agent loop each turn. Returns the oldest queued signal or null.
   * When `recipient` is given, returns only the oldest signal addressed to it (plus
   * unaddressed shared items); signals addressed to other recipients stay queued, so one
   * shared gateway can serve N peer loops. Omit ⇒ legacy FIFO drain (any signal).
   */
  async nextSignal(recipient?: string): Promise<RuntimeSignal | null> {
    const claim = await this.claimSignal(recipient)
    if (!claim) return null
    await this.ackSignal(claim)
    return claim.signal
  }

  /** Claim one visible signal without deleting it. Unacked claims are redelivered after expiry. */
  async claimSignal(recipient?: string, leaseMs = this.opts.defaultLeaseMs ?? 30_000): Promise<SignalClaim | null> {
    if (!Number.isFinite(leaseMs) || leaseMs <= 0) throw new RangeError("leaseMs must be positive")
    const now = this.opts.now?.() ?? Date.now()
    const idx = this.queue.findIndex(entry => {
      const visible = recipient === undefined
        || entry.signal.recipient === undefined
        || entry.signal.recipient === recipient
      const available = entry.lease === undefined || entry.lease.expiresAtMs <= now
      return visible && available
    })
    if (idx === -1) return null
    const entry = this.queue[idx]
    const token = `${entry.deliveryId}:lease-${++this.leaseSeq}`
    const expiresAtMs = now + leaseMs
    entry.lease = { token, expiresAtMs }
    return {
      deliveryId: entry.deliveryId,
      leaseToken: token,
      leaseExpiresAtMs: expiresAtMs,
      signal: entry.signal,
    }
  }

  /** Permanently remove the delivery iff the receipt still owns its current lease. */
  async ackSignal(receipt: SignalDeliveryReceipt): Promise<boolean> {
    const idx = this.currentLeaseIndex(receipt)
    if (idx === -1) return false
    this.queue.splice(idx, 1)
    return true
  }

  /** Release the current lease for immediate retry. Stale receipts are ignored. */
  async nackSignal(receipt: SignalDeliveryReceipt): Promise<boolean> {
    const idx = this.currentLeaseIndex(receipt)
    if (idx === -1) return false
    delete this.queue[idx].lease
    return true
  }

  // ── Push API ────────────────────────────────────────────────────────────────

  /** Register a listener that is called synchronously whenever a signal is emitted.
   *  Returns an unsubscribe function — long-lived consumers (e.g. a loop's
   *  `signalAwareSleeper`, re-registered per sleep) must call it or the listener leaks. */
  onSignal(listener: (sig: RuntimeSignal) => void): () => void {
    this.listeners.push(listener)
    return () => {
      const idx = this.listeners.indexOf(listener)
      if (idx !== -1) this.listeners.splice(idx, 1)
    }
  }

  /** Schedule a ScheduledPrompt to fire at its `runAtMs`. Idempotent by goal+time. */
  schedule(prompt: ScheduledPrompt): void {
    const key = `cron:${prompt.goal}:${prompt.runAtMs}`
    if (this.timers.has(key)) return

    const fire = () => {
      this.timers.delete(key)
      this.emit({
        kind: "scheduled",
        source: "cron",
        signalType: "job",
        urgency: "normal",
        dedupeKey: key,
        payload: { goal: prompt.goal, criteria: prompt.criteria, runAtMs: prompt.runAtMs, ...prompt.metadata },
      })
    }

    const delay = prompt.runAtMs - Date.now()
    if (delay <= 0) {
      fire()
    } else {
      this.timers.set(key, setTimeout(fire, delay))
    }
  }

  /** Cancel a scheduled prompt before it fires. */
  cancel(goal: string, runAtMs: number): void {
    const key = `cron:${goal}:${runAtMs}`
    const t = this.timers.get(key)
    if (t) { clearTimeout(t); this.timers.delete(key) }
  }

  /** Push a raw signal directly (e.g. from a webhook handler). */
  ingest(sig: RuntimeSignal): void {
    this.emit(sig)
  }

  /** Fan one logical signal out to a known recipient set. Each recipient gets one queue item. */
  broadcast(recipients: Iterable<string>, sig: RuntimeSignal): void {
    const seen = new Set<string>()
    for (const recipient of recipients) {
      if (!recipient || seen.has(recipient)) continue
      seen.add(recipient)
      this.emit({ ...sig, recipient })
    }
  }

  /** Number of signals currently buffered in the queue. */
  get depth(): number {
    return this.queue.length
  }

  /** Clear all pending timers. Call when shutting down to avoid process leaks. */
  destroy(): void {
    for (const t of this.timers.values()) clearTimeout(t)
    this.timers.clear()
    this.queue.length = 0
  }

  // ── Internal ────────────────────────────────────────────────────────────────

  private emit(sig: RuntimeSignal): void {
    this.queue.push({ deliveryId: `signal-${++this.deliverySeq}`, signal: sig })
    for (const listener of this.listeners) {
      try {
        listener(sig)
      } catch (cause) {
        reportObserverFailure(this.opts.onObserverError, {
          component: "SignalGateway",
          operation: "signal_listener",
          cause,
        })
      }
    }
  }

  private currentLeaseIndex(receipt: SignalDeliveryReceipt): number {
    return this.queue.findIndex(entry => entry.deliveryId === receipt.deliveryId
      && entry.lease?.token === receipt.leaseToken)
  }
}
