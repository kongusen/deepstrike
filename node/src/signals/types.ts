export type RuntimeSignalSource = "cron" | "gateway" | "heartbeat" | "custom"
export type RuntimeSignalType = "event" | "job" | "alert"
export type RuntimeSignalUrgency = "low" | "normal" | "high" | "critical"

export interface RuntimeSignal {
  source: RuntimeSignalSource
  signalType: RuntimeSignalType
  urgency: RuntimeSignalUrgency
  payload: Record<string, unknown>
  dedupeKey?: string
  /** @deprecated Use source/signalType/urgency directly. */
  kind?: "interrupt" | "scheduled" | "external"
  /** @deprecated Prefer explicit `urgency`. */
  priority?: number
}

export interface SignalSource {
  nextSignal(): Promise<RuntimeSignal | null>
}
