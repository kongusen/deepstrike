/**
 * L2 (TurnPolicy) — who reacts to a blackboard event. This is the one caller-customizable seam of a
 * `ReactiveSession`; the framework supplies a spanning default set (addressing / delegated /
 * deterministic-cyclic) plus combinators, so teams compose rather than hand-roll turn-taking.
 *
 * Stateful policies (e.g. `roundRobin`) must keep their cursor in `state`, which a `ReactiveSession`
 * persists to the group store so it survives stateless turns.
 */
import type { BlackboardEvent } from "./event-stream.js"

/** A peer the policy can choose to activate. */
export interface PeerView {
  personaId: string
  role?: string
  channels?: string[]
}

/**
 * Decide which peers react to `event`. Async to allow LLM-delegated selection. `state` is opaque,
 * persisted across turns by the session (for cursors etc.); mutate-and-return it.
 */
export type TurnPolicy = (
  event: BlackboardEvent,
  peers: PeerView[],
  state: Record<string, unknown>,
) => string[] | Promise<string[]>

const idsOf = (peers: PeerView[]) => peers.map(p => p.personaId)

/** Addressing: react iff the event names the peer (its `audience`, or its id/role in the payload). */
export function reactByMention(): TurnPolicy {
  return (event, peers) => {
    const hay = typeof event.payload === "string" ? event.payload : JSON.stringify(event.payload ?? "")
    return idsOf(peers).filter(id => {
      const peer = peers.find(p => p.personaId === id)!
      if (event.audience?.includes(id)) return true
      if (hay.includes(id)) return true
      return peer.role !== undefined && hay.includes(peer.role)
    })
  }
}

/**
 * Delegated: a designated persona (or fn) decides who reacts. The most flexible escape hatch — the
 * selector can be an LLM call. `select` receives the event + candidate peers and returns chosen ids.
 */
export function directorDriven(
  directorId: string,
  select: (event: BlackboardEvent, peers: PeerView[]) => string[] | Promise<string[]>,
): TurnPolicy {
  return async (event, peers) => {
    const chosen = await select(event, peers)
    const valid = new Set(idsOf(peers))
    return chosen.filter(id => valid.has(id) && id !== directorId)
  }
}

/** Deterministic: cycle through peers in order, one per event. Cursor persisted in `state`. */
export function roundRobin(): TurnPolicy {
  return (event, peers, state) => {
    if (peers.length === 0) return []
    const cursor = typeof state.rrCursor === "number" ? state.rrCursor : 0
    const idx = cursor % peers.length
    state.rrCursor = cursor + 1
    return [peers[idx].personaId]
  }
}

/** Combinator: first policy that selects a non-empty set wins (e.g. mention, else director). */
export function firstNonEmpty(...policies: TurnPolicy[]): TurnPolicy {
  return async (event, peers, state) => {
    for (const p of policies) {
      const chosen = await p(event, peers, state)
      if (chosen.length > 0) return chosen
    }
    return []
  }
}

/** Combinator: union of all policies' selections (deduped, order-stable). */
export function union(...policies: TurnPolicy[]): TurnPolicy {
  return async (event, peers, state) => {
    const out = new Set<string>()
    for (const p of policies) for (const id of await p(event, peers, state)) out.add(id)
    return [...out]
  }
}
