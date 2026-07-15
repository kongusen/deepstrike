import type { RuntimeSignal } from "./types.js"

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
      dedupeKey: `cron:${this.goal}:${this.runAtMs}`,
      payload: { goal: this.goal, criteria: this.criteria, runAtMs: this.runAtMs, ...this.metadata },
    }
  }
}
