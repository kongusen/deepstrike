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
}

export interface SignalSource {
  claimSignal(recipient?: string, leaseMs?: number): Promise<SignalClaim | null>
  ackSignal(receipt: SignalDeliveryReceipt): Promise<boolean>
  nackSignal(receipt: SignalDeliveryReceipt): Promise<boolean>
}

/** Opaque proof that one consumer currently owns a leased signal delivery. */
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
