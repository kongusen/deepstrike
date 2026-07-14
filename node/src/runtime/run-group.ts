/**
 * L1 (RunGroup) — a governance domain shared by N peer agent sessions of one logical run.
 *
 * The kernel (execution vehicle) is ephemeral and torn down between stateless turns, so the
 * cumulative budget + membership that must span the whole group live outside any vehicle: in a
 * `GroupBudgetStore`. A reservable store atomically holds capacity for each member, seeds its kernel
 * from settled usage plus earlier reservations, and settles actual consumption when the member ends.
 * Legacy stores retain read/charge accounting but cannot enforce a concurrent group quota.
 * Per spec §2.5, only *cumulative* budget is shared this way; instantaneous concurrency stays
 * vehicle-scoped.
 *
 * Two built-in stores:
 * - `InMemoryGroupBudgetStore` — process-local atomic reservations; fine for one replica / tests.
 * - `SessionLogGroupBudgetStore` — persists the ledger + membership to any `SessionLog` (fold-on-read
 *   under a group-anchor key). It is accounting-only because SessionLog has no compare-and-set API.
 */
import type { SessionLog } from "./session-log.js"
import { randomUUID } from "node:crypto"

/** Cumulative resources spent across a run group. */
export interface GroupLedger {
  /** ③ loop-agent rounds completed across the group (seeds the pacing trap's max_rounds). */
  roundsCompleted?: number
  /** Total tokens spent by all members. */
  tokensSpent: number
  /** Total sub-agents spawned by all members (running + completed). */
  subagentsSpawned: number
}

/** A member's contribution to charge back to the group ledger. */
// (rounds: ③ loop-agent — one per completed round.)
export interface GroupCharge {
  tokens?: number
  subagents?: number
  /** ③ loop-agent: completed rounds to add to the group's round count. */
  rounds?: number
}

export interface GroupBudgetRequest {
  /** Group-wide hard limits. An omitted axis is accounting-only. */
  limits: GroupCharge
  /** Maximum capacity this member wants to hold for its lifetime. */
  requested: GroupCharge
}

export interface GroupBudgetReservation {
  id: string
  groupId: string
  memberId: string
  /** Settled usage plus reservations held by earlier members; seed the kernel from this ledger. */
  ledger: GroupLedger
  /** Capacity granted to this member. May be lower than requested when the group is nearly full. */
  granted: GroupCharge
}

/** A persona session that participated in the logical run (process-table lineage). */
export interface GroupMember {
  sessionId: string
  role?: string
  /** W-N5: what this member IS in the lineage — a `"peer"` persona (ReactiveSession.addPeer) vs a
   *  `"vehicle"` session (run()/runWorkflow envelopes, workflow-node children, loop iterations).
   *  `ReactiveSession.resume()` rebuilds the peer set from `"peer"` members only, so DAG-in-Peer
   *  usage can't resurrect phantom `wf-node*` personas. Absent (legacy) = unknown. */
  kind?: "peer" | "vehicle"
}

export interface GroupBudgetStore {
  /** Cumulative spend across the group so far. */
  read(groupId: string): GroupLedger | Promise<GroupLedger>
  /** Add a member's spend to the group's cumulative totals. */
  charge(groupId: string, delta: GroupCharge): void | Promise<void>
  /** Register a persona session as a member of the group (idempotent by sessionId). */
  join(groupId: string, member: GroupMember): void | Promise<void>
  /** All persona sessions of the logical run — the cross-invocation lineage (R2). */
  members(groupId: string): GroupMember[] | Promise<GroupMember[]>
}

/** Transactional capability required for concurrent quota enforcement. */
export interface ReservableGroupBudgetStore extends GroupBudgetStore {
  reserve(
    groupId: string,
    request: GroupBudgetRequest & { memberId: string },
  ): GroupBudgetReservation | Promise<GroupBudgetReservation>
  /** Idempotently replace a reservation with actual usage. */
  settle(groupId: string, reservationId: string, actual: GroupCharge): void | Promise<void>
  /** Idempotently discard an unused reservation. */
  release(groupId: string, reservationId: string): void | Promise<void>
}

export function isReservableGroupBudgetStore(
  store: GroupBudgetStore,
): store is ReservableGroupBudgetStore {
  const candidate = store as Partial<ReservableGroupBudgetStore>
  return typeof candidate.reserve === "function" &&
    typeof candidate.settle === "function" &&
    typeof candidate.release === "function"
}

/** Process-local default store. One ledger + member set per group id. */
export class InMemoryGroupBudgetStore implements ReservableGroupBudgetStore {
  private readonly ledgers = new Map<string, GroupLedger>()
  private readonly memberships = new Map<string, Map<string, GroupMember>>()
  private readonly reservations = new Map<string, Map<string, GroupBudgetReservation>>()

  read(groupId: string): GroupLedger {
    const ledger = this.ledgers.get(groupId)
    return ledger
      ? { ...ledger }
      : { tokensSpent: 0, subagentsSpawned: 0, roundsCompleted: 0 }
  }

  charge(groupId: string, delta: GroupCharge): void {
    const cur = this.read(groupId)
    this.ledgers.set(groupId, {
      tokensSpent: cur.tokensSpent + Math.max(0, delta.tokens ?? 0),
      subagentsSpawned: cur.subagentsSpawned + Math.max(0, delta.subagents ?? 0),
      roundsCompleted: (cur.roundsCompleted ?? 0) + Math.max(0, delta.rounds ?? 0),
    })
  }

  join(groupId: string, member: GroupMember): void {
    if (!this.memberships.has(groupId)) this.memberships.set(groupId, new Map())
    // First join wins (idempotent by sessionId) — the same contract as SessionLogGroupBudgetStore.
    // A persona registered as "peer" then re-joining through its own run() as "vehicle" must not
    // lose its peer tag (W-N5), and the two stores must agree on which record survives.
    const members = this.memberships.get(groupId)!
    if (!members.has(member.sessionId)) members.set(member.sessionId, member)
  }

  members(groupId: string): GroupMember[] {
    return [...(this.memberships.get(groupId)?.values() ?? [])]
  }

  reserve(
    groupId: string,
    request: GroupBudgetRequest & { memberId: string },
  ): GroupBudgetReservation {
    const settled = this.read(groupId)
    const held = [...(this.reservations.get(groupId)?.values() ?? [])].reduce<GroupLedger>(
      (sum, reservation) => ({
        tokensSpent: sum.tokensSpent + (reservation.granted.tokens ?? 0),
        subagentsSpawned: sum.subagentsSpawned + (reservation.granted.subagents ?? 0),
        roundsCompleted: (sum.roundsCompleted ?? 0) + (reservation.granted.rounds ?? 0),
      }),
      { tokensSpent: 0, subagentsSpawned: 0, roundsCompleted: 0 },
    )
    const ledger: GroupLedger = {
      tokensSpent: settled.tokensSpent + held.tokensSpent,
      subagentsSpawned: settled.subagentsSpawned + held.subagentsSpawned,
      roundsCompleted: (settled.roundsCompleted ?? 0) + (held.roundsCompleted ?? 0),
    }
    const grant = (requested = 0, limit: number | undefined, used: number): number =>
      Math.max(0, Math.min(Math.max(0, requested), limit === undefined ? requested : limit - used))
    const reservation: GroupBudgetReservation = {
      id: randomUUID(),
      groupId,
      memberId: request.memberId,
      ledger,
      granted: {
        tokens: grant(request.requested.tokens, request.limits.tokens, ledger.tokensSpent),
        subagents: grant(request.requested.subagents, request.limits.subagents, ledger.subagentsSpawned),
        rounds: grant(request.requested.rounds, request.limits.rounds, ledger.roundsCompleted ?? 0),
      },
    }
    if (!this.reservations.has(groupId)) this.reservations.set(groupId, new Map())
    this.reservations.get(groupId)!.set(reservation.id, reservation)
    return reservation
  }

  settle(groupId: string, reservationId: string, actual: GroupCharge): void {
    const reservations = this.reservations.get(groupId)
    if (!reservations?.delete(reservationId)) return
    if (reservations.size === 0) this.reservations.delete(groupId)
    this.charge(groupId, actual)
  }

  release(groupId: string, reservationId: string): void {
    const reservations = this.reservations.get(groupId)
    if (!reservations?.delete(reservationId)) return
    if (reservations.size === 0) this.reservations.delete(groupId)
  }
}

/**
 * Persists the accounting ledger + membership to a `SessionLog`, keyed by a group-anchor session whose id
 * is the group id. Budget/membership rebuild by folding `group_budget_charged` / `group_member_joined`
 * events on read (spec §2.4). Durable across replicas, but not a concurrent quota enforcer: use a
 * `ReservableGroupBudgetStore` backed by a transactional database/Redis script for that contract.
 */
export class SessionLogGroupBudgetStore implements GroupBudgetStore {
  constructor(private readonly log: SessionLog) {}

  async read(groupId: string): Promise<GroupLedger> {
    let tokensSpent = 0
    let subagentsSpawned = 0
    let roundsCompleted = 0
    for (const { event } of await this.log.read(groupId)) {
      if (event.kind === "group_budget_charged") {
        roundsCompleted += (event as { rounds?: number }).rounds ?? 0
        tokensSpent += event.tokens
        subagentsSpawned += event.subagents
      }
    }
    return { tokensSpent, subagentsSpawned, roundsCompleted }
  }

  async charge(groupId: string, delta: GroupCharge): Promise<void> {
    await this.log.append(groupId, {
      kind: "group_budget_charged",
      ...(delta.rounds !== undefined ? { rounds: delta.rounds } : {}),
      tokens: Math.max(0, delta.tokens ?? 0),
      subagents: Math.max(0, delta.subagents ?? 0),
    })
  }

  async join(groupId: string, member: GroupMember): Promise<void> {
    // Idempotent: don't grow the log with duplicate joins for the same session.
    const existing = await this.members(groupId)
    if (existing.some(m => m.sessionId === member.sessionId)) return
    await this.log.append(groupId, {
      kind: "group_member_joined",
      session_id: member.sessionId,
      ...(member.role ? { role: member.role } : {}),
      ...(member.kind ? { member_kind: member.kind } : {}),
    })
  }

  async members(groupId: string): Promise<GroupMember[]> {
    const seen = new Map<string, GroupMember>()
    for (const { event } of await this.log.read(groupId)) {
      if (event.kind === "group_member_joined") {
        seen.set(event.session_id, {
          sessionId: event.session_id,
          role: event.role,
          ...(event.member_kind ? { kind: event.member_kind } : {}),
        })
      }
    }
    return [...seen.values()]
  }
}

/**
 * One member's budget lifecycle. Reservable stores enforce concurrent admission; legacy stores are
 * explicitly accounting-only and retain the previous read/charge behavior during migration.
 */
export class GroupBudgetScope {
  private closed = false

  private constructor(
    private readonly group: RunGroup,
    readonly ledger: GroupLedger,
    readonly granted: GroupCharge,
    readonly mode: "reserved" | "accounting",
    private readonly reservation?: GroupBudgetReservation,
  ) {}

  static async open(
    group: RunGroup,
    member: GroupMember,
    request?: GroupBudgetRequest,
  ): Promise<GroupBudgetScope> {
    await group.budgetStore.join(group.id, member)
    if (request && isReservableGroupBudgetStore(group.budgetStore)) {
      const reservation = await group.budgetStore.reserve(group.id, {
        ...request,
        memberId: member.sessionId,
      })
      return new GroupBudgetScope(group, reservation.ledger, reservation.granted, "reserved", reservation)
    }
    return new GroupBudgetScope(
      group,
      await group.budgetStore.read(group.id),
      { ...request?.requested },
      "accounting",
    )
  }

  async settle(actual: GroupCharge): Promise<void> {
    if (this.closed) return
    if (this.reservation && isReservableGroupBudgetStore(this.group.budgetStore)) {
      await this.group.budgetStore.settle(this.group.id, this.reservation.id, actual)
      this.closed = true
      return
    }
    if ((actual.tokens ?? 0) > 0 || (actual.subagents ?? 0) > 0 || (actual.rounds ?? 0) > 0) {
      await this.group.budgetStore.charge(this.group.id, actual)
    }
    this.closed = true
  }

  async release(): Promise<void> {
    if (this.closed) return
    if (this.reservation && isReservableGroupBudgetStore(this.group.budgetStore)) {
      await this.group.budgetStore.release(this.group.id, this.reservation.id)
    }
    this.closed = true
  }
}

/** Binds a runner to a governance domain: a stable group id + the store its members share. */
export interface RunGroup {
  /** Stable id for this logical run's governance domain; all members pass the same one. */
  id: string
  /** Shared cumulative-budget + membership store. */
  budgetStore: GroupBudgetStore
}
