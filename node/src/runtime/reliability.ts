/** A failure raised by an observer after the owning mutation has already committed. */
export interface ObserverFailure {
  component: string
  operation: string
  cause: unknown
  committed: true
}

export type ObserverErrorHandler = (failure: ObserverFailure) => void

/** Immutable identity and cancellation boundary for one runtime operation. */
export interface OperationContext {
  readonly runId: string
  readonly sessionId: string
  readonly agentId?: string
  readonly signal: AbortSignal
  readonly deadlineMs?: number
  readonly provenance?: Readonly<Record<string, string>>
}

export interface BackgroundTaskFailure {
  readonly label: string
  readonly operation: OperationContext
  readonly cause: unknown
}

export type BackgroundTaskErrorHandler = (failure: BackgroundTaskFailure) => void

/** Report an observer failure without allowing the reporter to change the committed result. */
export function reportObserverFailure(
  handler: ObserverErrorHandler | undefined,
  failure: Omit<ObserverFailure, "committed">,
): void {
  try {
    handler?.({ ...failure, committed: true })
  } catch {
    // The reporter is itself an observer. There is no additional semantic owner to fail here.
  }
}

/**
 * Serialize async mutations by owner key while allowing unrelated keys to proceed concurrently.
 * The queued tail is always resolved, so one failed mutation cannot poison later operations.
 */
export class KeyedSerialExecutor {
  private readonly tails = new Map<string, Promise<void>>()

  async run<T>(key: string, operation: () => Promise<T>): Promise<T> {
    const previous = this.tails.get(key) ?? Promise.resolve()
    let release!: () => void
    const gate = new Promise<void>(resolve => { release = resolve })
    const tail = previous.then(() => gate)
    this.tails.set(key, tail)

    await previous
    try {
      return await operation()
    } finally {
      release()
      void tail.then(() => {
        if (this.tails.get(key) === tail) this.tails.delete(key)
      })
    }
  }
}

/** Owns best-effort asynchronous work for exactly one operation. */
export class ManagedTaskScope {
  private readonly tasks = new Set<Promise<void>>()
  private readonly controller = new AbortController()
  private closed = false
  readonly operation: OperationContext

  constructor(
    operation: OperationContext,
    private readonly onTaskError?: BackgroundTaskErrorHandler,
  ) {
    this.operation = {
      ...operation,
      signal: AbortSignal.any([operation.signal, this.controller.signal]),
    }
  }

  get pending(): number {
    return this.tasks.size
  }

  spawn(
    label: string,
    work: (operation: OperationContext) => Promise<void> | void,
  ): void {
    if (this.closed) throw new Error("task scope is closed")
    const task = Promise.resolve()
      .then(() => work(this.operation))
      .catch(cause => {
        try {
          this.onTaskError?.({ label, operation: this.operation, cause })
        } catch {
          // Failure reporting is observational and cannot own the task result.
        }
      })
      .finally(() => { this.tasks.delete(task) })
    this.tasks.add(task)
  }

  async drain(): Promise<void> {
    this.closed = true
    await Promise.all([...this.tasks])
  }

  async cancel(reason?: unknown): Promise<void> {
    this.controller.abort(reason)
    await this.drain()
  }
}
