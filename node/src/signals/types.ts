export interface RuntimeSignal {
  kind: "interrupt" | "scheduled" | "external"
  payload: Record<string, unknown>
  priority?: number
}

export interface SignalSource {
  nextSignal(): Promise<RuntimeSignal | null>
}
