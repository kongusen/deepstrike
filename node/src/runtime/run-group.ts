/**
 * L1 (RunGroup) — a governance domain shared by N peer agent sessions of one logical run.
 *
 * The kernel (execution vehicle) is ephemeral and torn down between stateless turns, so the
 * cumulative budget + membership that must span the whole group live outside any vehicle: in a
 * `GroupBudgetStore`. Each member's run is seeded at boot with the group's accumulated spend (tokens
 * + sub-agent spawns) so the run-level token cap and the cumulative spawn cap are enforced across all
 * members, registers itself as a member (lineage), and charges its own consumption back when it ends.
 * Per spec §2.5, only *cumulative* budget is shared this way; instantaneous concurrency stays
 * vehicle-scoped.
 *
 * Two built-in stores:
 * - `InMemoryGroupBudgetStore` — process-local; fine for a single replica / tests.
 * - `SessionLogGroupBudgetStore` — persists the ledger + membership to any `SessionLog` (fold-on-read
 *   under a group-anchor key), so a logical run's governance + lineage survive process boundaries and
 *   span replicas when backed by a durable `SessionLog`.
 */
import type { SessionLog } from "./session-log.js"

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

/** Process-local default store. One ledger + member set per group id. */
export class InMemoryGroupBudgetStore implements GroupBudgetStore {
  private readonly ledgers = new Map<string, GroupLedger>()
  private readonly memberships = new Map<string, Map<string, GroupMember>>()

  read(groupId: string): GroupLedger {
    return this.ledgers.get(groupId) ?? { tokensSpent: 0, subagentsSpawned: 0, roundsCompleted: 0 }
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
}

/**
 * Persists the group ledger + membership to a `SessionLog`, keyed by a group-anchor session whose id
 * is the group id. Budget/membership rebuild by folding `group_budget_charged` / `group_member_joined`
 * events on read (spec §2.4). Durable + replica-spanning when the underlying `SessionLog` is.
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

/** Binds a runner to a governance domain: a stable group id + the store its members share. */
export interface RunGroup {
  /** Stable id for this logical run's governance domain; all members pass the same one. */
  id: string
  /** Shared cumulative-budget + membership store. */
  budgetStore: GroupBudgetStore
}
