/**
 * L2 (Blackboard) — a shared, append-only event stream that N peer agent sessions of one logical run
 * observe. This is the pluggable storage seam (like `SessionLog`): the default `InMemoryEventStream`
 * is process-local; back it with Postgres/Redis to span replicas/restarts.
 *
 * Visibility (spec §6.1): events are shared by default. Optional `channel` / `audience` tags scope an
 * event to a subset of personas, enforced at the framework boundary (`readSince(seq, viewer)` + the
 * `read_recent` tool) — context isolation, not convention.
 */

/** One entry on the shared blackboard. `channel`/`audience` are optional visibility scoping. */
export interface BlackboardEvent {
  seq: number
  payload: unknown
  /** Emitting persona id (or external source), for audit / `reactByMention`. */
  source?: string
  /** Channel this event belongs to; only personas subscribed to it see it. Omit ⇒ all see it. */
  channel?: string
  /** Explicit recipient persona ids; only they see it. Omit ⇒ all see it (subject to `channel`). */
  audience?: string[]
}

/** A reader's identity for visibility filtering. */
export interface EventViewer {
  personaId: string
  /** Channels this persona is subscribed to. */
  channels?: string[]
}

/** Default full-share visibility rule (spec §6.1). */
export function isVisibleTo(event: Pick<BlackboardEvent, "channel" | "audience">, viewer: EventViewer): boolean {
  if (event.audience === undefined && event.channel === undefined) return true
  if (event.audience?.includes(viewer.personaId)) return true
  if (event.channel !== undefined && viewer.channels?.includes(event.channel)) return true
  return false
}

export interface EventStream {
  /** Append an event; returns it stamped with its assigned `seq`. */
  append(event: Omit<BlackboardEvent, "seq">): Promise<BlackboardEvent>
  /** Events after `seq`. With a `viewer`, only those visible to it (default: all). */
  readSince(seq: number, viewer?: EventViewer): Promise<BlackboardEvent[]>
  /** Notify a listener on each appended event. Returns an unsubscribe fn. */
  subscribe(cb: (e: BlackboardEvent) => void): () => void
}

/** Process-local default blackboard. */
export class InMemoryEventStream implements EventStream {
  private readonly events: BlackboardEvent[] = []
  private readonly listeners = new Set<(e: BlackboardEvent) => void>()

  async append(event: Omit<BlackboardEvent, "seq">): Promise<BlackboardEvent> {
    const stamped: BlackboardEvent = { ...event, seq: this.events.length }
    this.events.push(stamped)
    for (const l of this.listeners) l(stamped)
    return stamped
  }

  async readSince(seq: number, viewer?: EventViewer): Promise<BlackboardEvent[]> {
    const after = this.events.filter(e => e.seq > seq)
    return viewer ? after.filter(e => isVisibleTo(e, viewer)) : after
  }

  subscribe(cb: (e: BlackboardEvent) => void): () => void {
    this.listeners.add(cb)
    return () => this.listeners.delete(cb)
  }
}
