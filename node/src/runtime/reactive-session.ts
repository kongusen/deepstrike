/**
 * L2 (ReactiveSession) — the user-facing primitive for "N peer agents over a shared event stream"
 * (spec §6). It composes the lower layers so teams don't hand-roll the pattern:
 *   - L1 `RunGroup`     — shared governance domain (cumulative budget + lineage) across the personas.
 *   - L0 `SignalGateway`— recipient-routed signals (targeted `interrupt` / `broadcast`).
 *   - `EventStream`     — the shared blackboard (pluggable; default in-memory).
 *   - `TurnPolicy`      — who reacts to each event (the one caller-customizable seam).
 *
 * Stateless-friendly: `emit` can run inside an HTTP handler; each persona's turn is a normal
 * `run({sessionId})` whose continuity comes from its `SessionLog`, and `resume()` rebuilds the peer
 * set from the persisted `RunGroup` membership — no hot in-process loop required.
 */
import type { RuntimeRunner } from "./runner.js"
import { collectText } from "./runner.js"
import type { RunGroup } from "./run-group.js"
import type { SignalSource, RuntimeSignal } from "../signals/types.js"
import { SignalGateway } from "../os/public.js"
import type { BlackboardEvent, EventStream, EventViewer } from "./event-stream.js"
import { InMemoryEventStream, isVisibleTo } from "./event-stream.js"
import type { TurnPolicy, PeerView } from "./turn-policy.js"
import { tool } from "../tools/index.js"
import type { RegisteredTool } from "../tools/index.js"

/** Per-persona registration: its base reaction goal, role, and channel subscriptions. */
export interface ReactivePeerSpec {
  goal?: string
  role?: string
  channels?: string[]
}

/** What the caller appends to the blackboard via `emit`. */
export interface EmitEvent {
  payload: unknown
  source?: string
  channel?: string
  audience?: string[]
}

export interface ReactiveSessionOptions {
  /** Shared governance domain — all personas run under it (L1). */
  runGroup: RunGroup
  /** Who reacts to each event (L2). */
  turnPolicy: TurnPolicy
  /** Shared blackboard. Defaults to a process-local `InMemoryEventStream`. */
  eventStream?: EventStream
  /** Shared signal gateway for targeted interrupt / broadcast (L0). Defaults to a fresh one. */
  signalGateway?: SignalGateway
  /**
   * Build a runner for a persona, wiring in the shared governance + signal routing. The app owns the
   * provider / execution plane / tools; spread `shared` into the `RuntimeRunner` options and register
   * `readRecentTool(shared.eventStream, viewer)` so the persona can read the blackboard.
   */
  makeRunner: (
    personaId: string,
    shared: { runGroup: RunGroup; signalSource: SignalSource; eventStream: EventStream },
  ) => RuntimeRunner
  /** Goal for a persona's reactive turn. Defaults to a generic "react to the blackboard" prompt. */
  goalFor?: (personaId: string, event: BlackboardEvent) => string
}

/** A persona's reaction to an emitted event. */
export interface Reaction {
  personaId: string
  output: string
}

export class ReactiveSession {
  private readonly peerSpecs = new Map<string, ReactivePeerSpec>()
  private readonly runners = new Map<string, RuntimeRunner>()
  private readonly policyState: Record<string, unknown> = {}
  private readonly eventStream: EventStream
  private readonly gateway: SignalGateway

  constructor(private readonly opts: ReactiveSessionOptions) {
    this.eventStream = opts.eventStream ?? new InMemoryEventStream()
    this.gateway = opts.signalGateway ?? new SignalGateway()
  }

  /** Register a peer persona and record it in the group membership (lineage). */
  addPeer(personaId: string, spec: ReactivePeerSpec = {}): void {
    this.peerSpecs.set(personaId, spec)
    void this.opts.runGroup.budgetStore.join(this.opts.runGroup.id, { sessionId: personaId, role: spec.role })
  }

  peers(): string[] {
    return [...this.peerSpecs.keys()]
  }

  blackboard(): EventStream {
    return this.eventStream
  }

  /**
   * Append an event to the blackboard, ask the `TurnPolicy` which (visible) peers react, and drive one
   * turn for each — returning their outputs. Each turn runs under the shared `RunGroup` governance.
   */
  async emit(event: EmitEvent): Promise<Reaction[]> {
    const bbEvent = await this.eventStream.append(event)

    const candidates: PeerView[] = [...this.peerSpecs.entries()]
      .map(([personaId, spec]) => ({ personaId, role: spec.role, channels: spec.channels }))
      // Only personas that can actually see the event are eligible to react.
      .filter(p => isVisibleTo(bbEvent, p as EventViewer))

    const chosen = await this.opts.turnPolicy(bbEvent, candidates, this.policyState)
    const eligible = new Set(candidates.map(p => p.personaId))

    const reactions: Reaction[] = []
    for (const personaId of chosen) {
      if (!eligible.has(personaId)) continue
      reactions.push({ personaId, output: await this.driveTurn(personaId, bbEvent) })
    }
    return reactions
  }

  /** Targeted preemption: deliver a critical signal to one persona's loop only (L0 recipient routing). */
  async interrupt(personaId: string, signal: Partial<RuntimeSignal> & { payload?: Record<string, unknown> }): Promise<void> {
    this.gateway.ingest({
      source: "gateway",
      signalType: "alert",
      urgency: "critical",
      payload: signal.payload ?? {},
      ...signal,
      recipient: personaId,
    })
  }

  /** Broadcast a signal to every persona (each sees it on its next turn). */
  async broadcast(signal: Partial<RuntimeSignal> & { payload?: Record<string, unknown> }): Promise<void> {
    this.gateway.ingest({
      source: "gateway",
      signalType: "event",
      urgency: "normal",
      payload: signal.payload ?? {},
      ...signal,
      recipient: undefined,
    })
  }

  private getRunner(personaId: string): RuntimeRunner {
    let runner = this.runners.get(personaId)
    if (!runner) {
      runner = this.opts.makeRunner(personaId, {
        runGroup: this.opts.runGroup,
        signalSource: this.gateway,
        eventStream: this.eventStream,
      })
      this.runners.set(personaId, runner)
    }
    return runner
  }

  private async driveTurn(personaId: string, event: BlackboardEvent): Promise<string> {
    const runner = this.getRunner(personaId)
    const goal =
      this.opts.goalFor?.(personaId, event) ??
      this.peerSpecs.get(personaId)?.goal ??
      "React to the latest events on the shared blackboard."
    // run() with the persona's stable sessionId replays its prior turns from the SessionLog, so this
    // is continuity-preserving whether or not the persona has acted before (stateless-handler safe).
    return collectText(runner.run({ sessionId: personaId, goal }))
  }

  /**
   * Rebuild a session from a persisted `RunGroup`: load its members (lineage) as peers. The blackboard
   * continuity comes from the (persistent) `EventStream`. Turn-policy cursor state is not restored.
   */
  static async resume(
    opts: ReactiveSessionOptions & { peerSpecs?: Record<string, ReactivePeerSpec> },
  ): Promise<ReactiveSession> {
    const session = new ReactiveSession(opts)
    for (const member of await opts.runGroup.budgetStore.members(opts.runGroup.id)) {
      session.peerSpecs.set(member.sessionId, opts.peerSpecs?.[member.sessionId] ?? { role: member.role })
    }
    return session
  }
}

/**
 * A `read_recent` tool a persona uses to read the shared blackboard, scoped to what it may see. Register
 * one per persona inside `makeRunner`. `viewer` is the reading persona (id + subscribed channels).
 */
export function readRecentTool(eventStream: EventStream, viewer: EventViewer): RegisteredTool {
  return tool(
    "read_recent",
    "Read recent events from the shared blackboard visible to you (optionally a single channel).",
    {
      type: "object",
      properties: {
        since_seq: { type: "number", description: "Only events after this seq (default: from the start)." },
        channel: { type: "string", description: "Restrict to one channel you subscribe to." },
      },
    },
    async (args) => {
      const sinceSeq = typeof args.since_seq === "number" ? args.since_seq : -1
      const channel = typeof args.channel === "string" ? args.channel : undefined
      const events = await eventStream.readSince(sinceSeq, viewer)
      const filtered = channel ? events.filter(e => e.channel === channel) : events
      return JSON.stringify(filtered.map(e => ({ seq: e.seq, source: e.source, channel: e.channel, payload: e.payload })))
    },
  )
}
