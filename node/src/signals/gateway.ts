import type { RuntimeSignal } from "./types.js"
import type { ScheduledPrompt } from "./scheduled.js"

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function loadKernel(): Promise<any> {
  return import("@deepstrike/core")
}

/**
 * SignalGateway — the entry point for all external signals into the agent.
 *
 * Responsibilities:
 * - Cron scheduling: fires ScheduledPrompts at the right time (deduplicated)
 * - Webhook ingestion: converts raw external payloads to RuntimeSignal
 * - Routes all signals through the kernel SignalRouter for priority + dedup
 *
 * Usage:
 *   const gateway = new SignalGateway()
 *   gateway.schedule(new ScheduledPrompt("summarize", Date.now() + 60_000))
 *   gateway.onSignal(sig => agent.ingestSignal(sig))
 */
export class SignalGateway {
  private timers = new Map<string, ReturnType<typeof setTimeout>>()
  private listeners: Array<(sig: RuntimeSignal) => void> = []
  private router: any = null

  async init(): Promise<void> {
    const kernel = await loadKernel()
    this.router = new kernel.SignalRouter(1024)
  }

  onSignal(listener: (sig: RuntimeSignal) => void): void {
    this.listeners.push(listener)
  }

  /** Schedule a ScheduledPrompt to fire at its runAtMs. Idempotent by goal. */
  schedule(prompt: ScheduledPrompt): void {
    const key = `cron:${prompt.goal}:${prompt.runAtMs}`
    if (this.timers.has(key)) return  // already scheduled

    const delay = prompt.runAtMs - Date.now()
    const fire = () => {
      this.timers.delete(key)
      this.emit({
        kind: "scheduled",
        payload: { goal: prompt.goal, criteria: prompt.criteria, runAtMs: prompt.runAtMs, ...prompt.metadata },
      })
    }

    if (delay <= 0) {
      fire()
    } else {
      this.timers.set(key, setTimeout(fire, delay))
    }
  }

  /** Cancel a scheduled prompt. */
  cancel(goal: string, runAtMs: number): void {
    const key = `cron:${goal}:${runAtMs}`
    const t = this.timers.get(key)
    if (t) { clearTimeout(t); this.timers.delete(key) }
  }

  /** Ingest a raw external signal (e.g. from a webhook handler). */
  ingest(sig: RuntimeSignal): void {
    this.emit(sig)
  }

  private emit(sig: RuntimeSignal): void {
    for (const l of this.listeners) l(sig)
  }

  destroy(): void {
    for (const t of this.timers.values()) clearTimeout(t)
    this.timers.clear()
  }
}
