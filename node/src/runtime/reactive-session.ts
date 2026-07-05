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

/**
 * How a persona executes one reactive turn. The default body is a single `runner.run(...)` agent turn;
 * override it to make a persona's turn a *different orchestration form* — e.g. drive a DAG via
 * `ctx.runner.runWorkflow(spec)` (DAG-in-Peer) or any composite. The runner is already wired to the
 * shared `RunGroup`, so whatever the body spawns stays under one governance domain. Must return the
 * persona's reaction text.
 */
export interface ReactorContext {
  personaId: string
  goal: string
  event: BlackboardEvent
  /** The persona's runner — wired to the shared RunGroup / signal gateway / blackboard. */
  runner: RuntimeRunner
}
export type ReactorTurn = (ctx: ReactorContext) => Promise<string>

/** Per-persona registration: its base reaction goal, role, channel subscriptions, and turn body. */
export interface ReactivePeerSpec {
  goal?: string
  role?: string
  channels?: string[]
  /**
   * Override this persona's turn body (the seam for composing other mechanisms into a peer). Defaults
   * to the session `reactWith`, then to a single `run()` agent turn. Use to make this peer's reaction
   * a workflow DAG, a nested ensemble, etc. — all under the shared `RunGroup`.
   */
  react?: ReactorTurn
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
  /**
   * Default turn body for peers that don't set their own `react`. Defaults to a single `run()` agent
   * turn. Override to make every peer's turn a different orchestration form (e.g. a workflow DAG).
   */
  reactWith?: ReactorTurn
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
    // W-N5: tagged "peer" so resume() can tell personas apart from vehicle sessions (workflow
    // envelopes / wf-node children / loop iterations) that share the same governance domain.
    void this.opts.runGroup.budgetStore.join(this.opts.runGroup.id, {
      sessionId: personaId,
      role: spec.role,
      kind: "peer",
    })
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
    // Turn-body seam (the DAG-in-Peer enabler): a persona's reaction can be any orchestration form,
    // not just a single agent turn. Per-peer `react` wins, then session `reactWith`, else the default
    // `run()`. Whatever the body drives (e.g. `runner.runWorkflow`) inherits the shared RunGroup.
    const react = this.peerSpecs.get(personaId)?.react ?? this.opts.reactWith
    if (react) return react({ personaId, goal, event, runner })
    // run() with the persona's stable sessionId replays its prior turns from the SessionLog, so this
    // is continuity-preserving whether or not the persona has acted before (stateless-handler safe).
    return collectText(runner.run({ sessionId: personaId, goal }))
  }

  /**
   * Rebuild a session from a persisted `RunGroup`: load its PEER members (lineage) as peers. The
   * blackboard continuity comes from the (persistent) `EventStream`. Turn-policy cursor state is not
   * restored. W-N5: vehicle members (workflow envelopes, `wf-node*` children, loop iterations) share
   * the governance domain but are NOT personas — resuming them as peers would resurrect phantoms.
   * A legacy membership with no kind tags falls back to resuming every member.
   */
  static async resume(
    opts: ReactiveSessionOptions & { peerSpecs?: Record<string, ReactivePeerSpec> },
  ): Promise<ReactiveSession> {
    const session = new ReactiveSession(opts)
    const members = await opts.runGroup.budgetStore.members(opts.runGroup.id)
    const anyTagged = members.some(m => m.kind !== undefined)
    for (const member of members) {
      if (anyTagged && member.kind !== "peer") continue
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
