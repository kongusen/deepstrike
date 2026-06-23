/**
 * L2 — EventStream visibility, TurnPolicy default set, and ReactiveSession orchestration (spec §6).
 */
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import {
  RuntimeRunner, InMemorySessionLog, LocalExecutionPlane,
  InMemoryGroupBudgetStore, InMemoryEventStream, isVisibleTo,
  reactByMention, directorDriven, roundRobin, firstNonEmpty,
  ReactiveSession, readRecentTool,
} from "../src/index.js"
import type { RunGroup, BlackboardEvent, PeerView, SignalSource } from "../src/index.js"

// ── EventStream visibility ──────────────────────────────────────────────────
describe("EventStream visibility (L2 §6.1)", () => {
  it("default full-share; channel/audience scope at the framework boundary", async () => {
    const s = new InMemoryEventStream()
    await s.append({ payload: "to-all" })
    await s.append({ payload: "to-coach", audience: ["coach", "learner"] })
    await s.append({ payload: "ch-a", channel: "a" })

    const coach = await s.readSince(-1, { personaId: "coach", channels: [] })
    expect(coach.map(e => e.payload)).toEqual(["to-all", "to-coach"]) // not ch-a (unsubscribed)

    const roleInA = await s.readSince(-1, { personaId: "role", channels: ["a"] })
    expect(roleInA.map(e => e.payload)).toEqual(["to-all", "ch-a"]) // not to-coach (not in audience)

    // No viewer ⇒ unfiltered.
    expect((await s.readSince(-1)).length).toBe(3)
  })

  it("isVisibleTo: untagged is universal", () => {
    expect(isVisibleTo({}, { personaId: "x" })).toBe(true)
    expect(isVisibleTo({ audience: ["y"] }, { personaId: "x" })).toBe(false)
    expect(isVisibleTo({ channel: "c" }, { personaId: "x", channels: ["c"] })).toBe(true)
  })
})

// ── TurnPolicy default set ──────────────────────────────────────────────────
describe("TurnPolicy default set (L2 §6.2.1)", () => {
  const peers: PeerView[] = [
    { personaId: "director", role: "director" },
    { personaId: "alice", role: "buyer" },
    { personaId: "bob", role: "seller" },
  ]
  const ev = (payload: unknown, audience?: string[]): BlackboardEvent => ({ seq: 0, payload, audience })

  it("reactByMention selects only named peers", async () => {
    expect(await reactByMention()(ev("hey alice"), peers, {})).toEqual(["alice"])
    expect(await reactByMention()(ev("x", ["bob"]), peers, {})).toEqual(["bob"])
  })

  it("directorDriven delegates selection and never picks the director", async () => {
    const policy = directorDriven("director", () => ["alice", "director"])
    expect(await policy(ev("?"), peers, {})).toEqual(["alice"])
  })

  it("roundRobin cycles deterministically via persisted cursor", async () => {
    const state: Record<string, unknown> = {}
    const policy = roundRobin()
    const seq = []
    for (let i = 0; i < 4; i++) seq.push((await policy(ev(i), peers, state))[0])
    expect(seq).toEqual(["director", "alice", "bob", "director"])
  })

  it("firstNonEmpty falls back when the first policy is empty", async () => {
    const policy = firstNonEmpty(reactByMention(), directorDriven("director", () => ["bob"]))
    expect(await policy(ev("nobody named"), peers, {})).toEqual(["bob"])
    expect(await policy(ev("alice here"), peers, {})).toEqual(["alice"])
  })
})

// ── ReactiveSession orchestration ───────────────────────────────────────────
class TextProvider implements LLMProvider {
  constructor(private readonly personaId: string) {}
  async complete(): Promise<Message> { return { role: "assistant", content: `${this.personaId}-ack`, toolCalls: [] } }
  async *stream(_c: RenderedContext, _t: ToolSchema[]): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: `${this.personaId}-ack` }
  }
}

function makeSession(turnPolicy: ReturnType<typeof reactByMention>): { session: ReactiveSession; store: InMemoryGroupBudgetStore } {
  const store = new InMemoryGroupBudgetStore()
  const runGroup: RunGroup = { id: "scenario", budgetStore: store }
  const eventStream = new InMemoryEventStream()
  const session = new ReactiveSession({
    runGroup,
    turnPolicy,
    eventStream,
    makeRunner: (personaId, shared) => {
      const plane = new LocalExecutionPlane()
      plane.register(readRecentTool(shared.eventStream, { personaId }))
      return new RuntimeRunner({
        provider: new TextProvider(personaId),
        sessionLog: new InMemorySessionLog(),
        executionPlane: plane,
        maxTokens: 4096,
        agentId: personaId,
        runGroup: shared.runGroup,
        signalSource: shared.signalSource as SignalSource,
      })
    },
  })
  return { session, store }
}

describe("ReactiveSession orchestration (L2 §6.2)", () => {
  it("emit drives only the peers the policy selects, under shared governance", async () => {
    const { session, store } = makeSession(reactByMention())
    session.addPeer("alice", { role: "buyer" })
    session.addPeer("bob", { role: "seller" })

    const reactions = await session.emit({ payload: "alice, your move", source: "director" })
    expect(reactions.map(r => r.personaId)).toEqual(["alice"])
    expect(reactions[0].output).toContain("alice-ack")

    // Both peers are recorded as group lineage; the shared ledger accrued alice's turn.
    expect((await store.members("scenario")).map(m => m.sessionId).sort()).toEqual(["alice", "bob"])
    expect(store.read("scenario").tokensSpent).toBeGreaterThan(0)
  })

  it("react seam overrides the turn body (DAG-in-Peer enabler, L2 §6.2)", async () => {
    const { session } = makeSession(reactByMention())
    session.addPeer("alice", {
      role: "buyer",
      react: async ({ personaId, runner, event }) => {
        expect(runner).toBeDefined()
        expect(event).toBeDefined()
        return `custom:${personaId}`
      },
    })
    session.addPeer("bob", { role: "seller" }) // default body (run())

    const reactions = await session.emit({ payload: "alice, your move", source: "director" })
    expect(reactions.map(r => r.personaId)).toEqual(["alice"])
    expect(reactions[0].output).toBe("custom:alice") // routed to the override, not run()
  })

  it("respects blackboard visibility: an unaddressed peer never reacts", async () => {
    const { session } = makeSession(roundRobin())
    session.addPeer("coach", { channels: [] })
    session.addPeer("role", { channels: ["a"] })
    // Event scoped to channel "a" — only "role" is eligible, so roundRobin can only pick it.
    const reactions = await session.emit({ payload: "scene", channel: "a" })
    expect(reactions.map(r => r.personaId)).toEqual(["role"])
  })

  it("resume rebuilds the peer set from persisted group membership", async () => {
    const { session, store } = makeSession(reactByMention())
    session.addPeer("director", { role: "director" })
    session.addPeer("npc", { role: "seller" })
    // Simulate a fresh process: rebuild from the (persisted) group store.
    const resumed = await ReactiveSession.resume({ ...(session as any).opts, runGroup: { id: "scenario", budgetStore: store } })
    expect(resumed.peers().sort()).toEqual(["director", "npc"])
  })
})
