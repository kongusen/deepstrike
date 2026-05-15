export interface RuntimeSignal {
  source: "cron" | "gateway" | "heartbeat" | "custom"
  signalType: "event" | "job" | "alert"
  urgency: "low" | "normal" | "high" | "critical"
  payload: Record<string, unknown>
  dedupeKey?: string
}

export interface SignalSource {
  nextSignal(): Promise<RuntimeSignal | null>
}

export class ScheduledPrompt {
  constructor(
    public readonly goal: string,
    public readonly runAtMs: number,
    public readonly criteria: string[] = [],
    public readonly metadata: Record<string, unknown> = {},
  ) {}

  toSignal(): RuntimeSignal {
    return {
      source: "cron",
      signalType: "job",
      urgency: "normal",
      payload: { goal: this.goal, criteria: this.criteria, runAtMs: this.runAtMs, ...this.metadata },
      dedupeKey: `scheduled-${this.runAtMs}`,
    }
  }
}
