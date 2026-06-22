import type { RuntimeSignal, SignalSource } from "./types.js"
import type { ScheduledPrompt } from "./scheduled.js"

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
export class SignalGateway implements SignalSource {
  private timers = new Map<string, ReturnType<typeof setTimeout>>()
  private queue: RuntimeSignal[] = []
  private listeners: Array<(sig: RuntimeSignal) => void> = []

  // ── SignalSource interface (pull model) ─────────────────────────────────────

  /**
   * Called by the agent loop each turn. Returns the oldest queued signal or null.
   * When `recipient` is given, returns only the oldest signal addressed to it (plus
   * unaddressed broadcasts); signals addressed to other recipients stay queued, so one
   * shared gateway can serve N peer loops. Omit ⇒ legacy FIFO drain (any signal).
   */
  async nextSignal(recipient?: string): Promise<RuntimeSignal | null> {
    if (recipient === undefined) return this.queue.shift() ?? null
    const idx = this.queue.findIndex(s => s.recipient === undefined || s.recipient === recipient)
    if (idx === -1) return null
    return this.queue.splice(idx, 1)[0]
  }

  // ── Push API ────────────────────────────────────────────────────────────────

  /** Register a listener that is called synchronously whenever a signal is emitted. */
  onSignal(listener: (sig: RuntimeSignal) => void): void {
    this.listeners.push(listener)
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
    this.queue.push(sig)
    for (const l of this.listeners) l(sig)
  }
}
