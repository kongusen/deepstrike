export interface RuntimeSignal {
  source: "cron" | "gateway" | "heartbeat" | "custom"
  signalType: "event" | "job" | "alert"
  urgency: "low" | "normal" | "high" | "critical"
  payload: Record<string, unknown>
  dedupeKey?: string
  /** Target a specific session loop. Omitted means a shared signal. */
  recipient?: string
  /** Absolute journal-clock deadline for optional urgency escalation. */
  deadlineMs?: number
  /** Merge with an unconsumed queued signal carrying the same key. */
  coalesceKey?: string
  /** Number of host signals deterministically represented by this signal. */
  coalescedCount?: number
}

export interface SignalSource {
  claimSignal(): Promise<SignalClaim | null>
  ackSignal(receipt: SignalDeliveryReceipt): Promise<boolean>
  nackSignal(receipt: SignalDeliveryReceipt): Promise<boolean>
}

export interface SignalDeliveryReceipt {
  deliveryId: string
  leaseToken: string
}

export interface SignalClaim extends SignalDeliveryReceipt {
  signalId: string
  deliveryAttempt: number
  signal: RuntimeSignal
  leaseExpiresAtMs: number
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
      coalescedCount: 1,
      dedupeKey: `scheduled-${this.runAtMs}`,
    }
  }
}
