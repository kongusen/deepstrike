/**
 * L1 (RunGroup) — cumulative token budget spans the governance domain (R2).
 *
 * N peer sessions of one logical run share a `GroupBudgetStore`. Each run is seeded at boot with the
 * group's cumulative spend, so the run-level `maxTotalTokens` cap is enforced across all members, not
 * per-vehicle. No `runGroup` ⇒ N=1, per-run budget (unchanged).
 */
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, InMemoryGroupBudgetStore, SessionLogGroupBudgetStore } from "../src/index.js"
import { tool } from "../src/tools/index.js"
import type { RunGroup } from "../src/index.js"

// Calls a tool on the first turn (so the loop continues to a scheduling boundary where the token
// budget is evaluated), then returns final text. A single-turn final-text response would complete
// before the budget axis is ever checked.
class ToolThenTextProvider implements LLMProvider {
  private turn = 0
  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }
  async *stream(_ctx: RenderedContext, _tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    this.turn += 1
    if (this.turn === 1) {
      yield { type: "tool_call", id: "call_1", name: "noop", arguments: {} }
    } else {
      yield { type: "text_delta", delta: "done" }
    }
  }
}

const noopTool = tool("noop", "does nothing", { type: "object", properties: {} }, () => "ok")

async function runToDone(
  runner: RuntimeRunner,
  sessionId: string,
  goal: string,
): Promise<{ status: string; totalTokens: number }> {
  let done = { status: "", totalTokens: 0 }
  for await (const evt of runner.run({ sessionId, goal })) {
    if ((evt as { type: string }).type === "done") {
      const e = evt as unknown as { status: string; totalTokens: number }
      done = { status: e.status, totalTokens: e.totalTokens }
    }
  }
  return done
}

function makeRunner(runGroup?: RunGroup, agentId?: string): RuntimeRunner {
  const plane = new LocalExecutionPlane()
  plane.register(noopTool)
  return new RuntimeRunner({
    provider: new ToolThenTextProvider(),
    sessionLog: new InMemorySessionLog(),
    executionPlane: plane,
    maxTokens: 4096,
    maxTotalTokens: 100_000, // run-level cap, comfortably above one short run
    agentId,
    runGroup,
  })
}

describe("RunGroup cumulative token budget (L1/R2)", () => {
  it("a member is seeded with the group's prior spend and hits the shared cap", async () => {
    const store = new InMemoryGroupBudgetStore()
    const group: RunGroup = { id: "scenario-1", budgetStore: store }

    // Simulate other members of the domain having already exhausted the 100k cap.
    store.charge(group.id, { tokens: 100_000 })

    // This member is seeded with the group's spend → even a tiny turn tips over → token_budget.
    const { status } = await runToDone(makeRunner(group), "director", "open the scene")
    expect(status).toBe("token_budget")
  })

  it("without a group, the same run completes (per-vehicle budget, unchanged)", async () => {
    const { status } = await runToDone(makeRunner(undefined), "solo", "open the scene")
    expect(status).toBe("completed")
  })

  it("charges each member's spend back so the group total accumulates", async () => {
    const store = new InMemoryGroupBudgetStore()
    const group: RunGroup = { id: "scenario-2", budgetStore: store }

    expect(store.read(group.id).tokensSpent).toBe(0)
    const r1 = await runToDone(makeRunner(group), "p1", "first beat")
    expect(r1.status).toBe("completed")
    expect(r1.totalTokens).toBeGreaterThan(0)
    // The run's local spend is now reflected in the shared domain total.
    expect(store.read(group.id).tokensSpent).toBe(r1.totalTokens)

    const r2 = await runToDone(makeRunner(group), "p2", "second beat")
    expect(store.read(group.id).tokensSpent).toBe(r1.totalTokens + r2.totalTokens)
  })

  // Kernel enforcement of the cumulative spawn cap (a member seeded at the cap is denied its spawn)
  // is covered by the kernel test `group_spawns_base_enforces_cumulative_spawn_cap`. Here we verify
  // the SDK-side ledger that seeds/charges it carries both axes independently.
  it("the group ledger accumulates tokens and sub-agent spawns independently", () => {
    const store = new InMemoryGroupBudgetStore()
    expect(store.read("g")).toEqual({ tokensSpent: 0, subagentsSpawned: 0, roundsCompleted: 0 })

    store.charge("g", { tokens: 100 })
    store.charge("g", { subagents: 2 })
    store.charge("g", { tokens: 50, subagents: 1 })

    expect(store.read("g")).toEqual({ tokensSpent: 150, subagentsSpawned: 3, roundsCompleted: 0 })
    // Distinct groups stay isolated.
    expect(store.read("other")).toEqual({ tokensSpent: 0, subagentsSpawned: 0, roundsCompleted: 0 })
  })

  it("tracks membership (lineage) across the personas of one logical run", async () => {
    const store = new InMemoryGroupBudgetStore()
    const group: RunGroup = { id: "scenario-3", budgetStore: store }
    await runToDone(makeRunner(group, "director"), "director", "beat 1")
    await runToDone(makeRunner(group, "role-npc"), "role-npc", "beat 2")
    await runToDone(makeRunner(group, "director"), "director", "beat 3") // rejoin is idempotent

    const members = await store.members(group.id)
    expect(members.map(m => m.sessionId).sort()).toEqual(["director", "role-npc"])
    expect(members.find(m => m.sessionId === "director")?.role).toBe("director")
  })

  it("SessionLogGroupBudgetStore persists ledger + membership across store instances", async () => {
    // A shared, durable SessionLog stands in for Postgres/Redis: a fresh store instance (e.g. a new
    // replica) rebuilds the same governance state by folding the persisted group-anchor events.
    const log = new InMemorySessionLog()
    const writer = new SessionLogGroupBudgetStore(log)
    await writer.join("run-x", { sessionId: "director", role: "director" })
    await writer.charge("run-x", { tokens: 1200, subagents: 2 })
    await writer.charge("run-x", { tokens: 800, subagents: 1 })

    const reader = new SessionLogGroupBudgetStore(log) // different instance, same log
    expect(await reader.read("run-x")).toEqual({ tokensSpent: 2000, subagentsSpawned: 3, roundsCompleted: 0 })
    expect((await reader.members("run-x")).map(m => m.sessionId)).toEqual(["director"])

    // Idempotent join: re-joining the same session does not duplicate the lineage entry.
    await reader.join("run-x", { sessionId: "director", role: "director" })
    expect((await reader.members("run-x")).length).toBe(1)
  })
})
