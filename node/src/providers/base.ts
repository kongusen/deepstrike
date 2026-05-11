export class CircuitBreaker {
  private failures = 0
  private openedAt: number | null = null

  constructor(
    private readonly openAfter: number = 5,
    private readonly resetAfter: number = 60_000,
  ) {}

  isOpen(): boolean {
    if (this.openedAt === null) return false
    if (Date.now() - this.openedAt >= this.resetAfter) {
      this.openedAt = null
      return false
    }
    return true
  }

  recordSuccess(): void {
    this.failures = 0
    this.openedAt = null
  }

  recordFailure(): void {
    this.failures++
    if (this.failures >= this.openAfter) this.openedAt = Date.now()
  }
}

export function normalizeToolCall(id: string, name: string, args: unknown): { id: string; name: string; arguments: string } | null {
  const n = String(name ?? "").trim()
  if (!n) return null
  let parsed: Record<string, unknown> = {}
  if (typeof args === "string") {
    try { parsed = JSON.parse(args || "{}") } catch { parsed = {} }
  } else if (args && typeof args === "object") {
    parsed = args as Record<string, unknown>
  }
  return { id: String(id ?? ""), name: n, arguments: JSON.stringify(parsed) }
}
