export interface RuntimeSignal {
  kind: "interrupt" | "scheduled" | "external"
  payload: Record<string, unknown>
  priority?: number
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
      kind: "scheduled",
      payload: { goal: this.goal, criteria: this.criteria, runAtMs: this.runAtMs, ...this.metadata },
    }
  }
}
