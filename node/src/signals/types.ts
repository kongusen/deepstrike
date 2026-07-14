export type RuntimeSignalSource = "cron" | "gateway" | "heartbeat" | "custom"
export type RuntimeSignalType = "event" | "job" | "alert"
export type RuntimeSignalUrgency = "low" | "normal" | "high" | "critical"

export interface RuntimeSignal {
  source: RuntimeSignalSource
  signalType: RuntimeSignalType
  urgency: RuntimeSignalUrgency
  payload: Record<string, unknown>
  dedupeKey?: string
  /** Target a specific session loop. Omitted means a shared item consumed by one eligible puller. */
  recipient?: string
  /** Optional pub/sub topic (carried through; multi-subscriber routing deferred). */
  topic?: string
  /** @deprecated Use source/signalType/urgency directly. */
  kind?: "interrupt" | "scheduled" | "external"
  /** @deprecated Prefer explicit `urgency`. */
  priority?: number
}

export interface SignalSource {
  /**
   * Pull the next pending signal. When `recipient` is given, return only signals
   * addressed to it (plus unaddressed shared items); other recipients' signals stay
   * queued. Omit ⇒ legacy FIFO drain (any signal).
   */
  nextSignal(recipient?: string): Promise<RuntimeSignal | null>
}

/** Opaque proof that one consumer currently owns a leased signal delivery. */
export interface SignalDeliveryReceipt {
  deliveryId: string
  leaseToken: string
}

export interface SignalClaim extends SignalDeliveryReceipt {
  signal: RuntimeSignal
  leaseExpiresAtMs: number
}

/** Additive lease capability for sources that can redeliver work after consumer failure. */
export interface LeasedSignalSource extends SignalSource {
  claimSignal(recipient?: string, leaseMs?: number): Promise<SignalClaim | null>
  ackSignal(receipt: SignalDeliveryReceipt): Promise<boolean>
  nackSignal(receipt: SignalDeliveryReceipt): Promise<boolean>
}

export function isLeasedSignalSource(source: SignalSource): source is LeasedSignalSource {
  const candidate = source as Partial<LeasedSignalSource>
  return typeof candidate.claimSignal === "function"
    && typeof candidate.ackSignal === "function"
    && typeof candidate.nackSignal === "function"
}
