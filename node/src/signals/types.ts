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
