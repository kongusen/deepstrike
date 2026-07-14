/** A failure raised by an observer after the owning mutation has already committed. */
export interface ObserverFailure {
  component: string
  operation: string
  cause: unknown
  committed: true
}

export type ObserverErrorHandler = (failure: ObserverFailure) => void

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
