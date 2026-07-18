/**
 * Regression: a nested vehicle (a sub-agent spawned by the SubAgentOrchestrator while its parent
 * already holds a RunGroup reservation) must NOT re-reserve the group's token axis. Before the fix
 * the child opened its GroupBudgetScope with the full per-vehicle request, the peer-competition
 * formula squeezed its grant to 0 tokens against the parent's held reservation, and the kernel's
 * configure_run then stripped the child's first-turn tool list — the child model saw "no tools".
 *
 * After the fix the orchestrator builds the child runner with `nestedGroupVehicle: true`, so the
 * child's scope opens with an EMPTY request (`{limits:{}, requested:{}}`) → granted `{}` (no axis).
 * The child joins for lineage/settlement only; the parent's held reservation can no longer squeeze
 * it, and its first-turn tools (here `noop`) survive.
 *
 * The parent kernel is injected (mirrors spawn-sub-agent-deny.test.ts) with `noop` mounted as a
 * capability so the kernel-computed spawn manifest carries it through `spec.capabilityFilter`. The
 * RecordingStore captures every reservation (mirrors run-group-budget.test.ts) so the child's
 * empty grant is directly assertable.
 */
import { getKernel } from "../src/kernel.js"
import {
  GroupBudgetScope,
  InMemoryGroupBudgetStore,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  type AgentRunSpec,
  type GroupBudgetRequest,
  type GroupBudgetReservation,
  type RunGroup,
  type StreamEvent,
} from "../src/index.js"
import type { LLMProvider, Message, RenderedContext, ToolSchema } from "../src/types.js"
import { tool } from "../src/tools/index.js"
import { capabilityCommandMount, capabilityTool } from "../src/runtime/kernel-step.js"
import { durableStartKernelV2, durableStepKernelV2 } from "./helpers/kernel-v2.js"

/** Records the tool names it is handed on every LLM call, then completes the turn with plain text. */
class RecordingProvider implements LLMProvider {
  readonly calls: string[][] = []
  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }
  async *stream(_ctx: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    this.calls.push(tools.map(t => t.name))
    yield { type: "text_delta", delta: "done" }
  }
}

/** Captures every reservation the store hands out so the child's empty grant is assertable. The
 *  record field is deliberately NOT named `reservations` — that collides with the base store's
 *  private `reservations` Map and breaks `super.reserve`. */
class RecordingStore extends InMemoryGroupBudgetStore {
  readonly recorded: GroupBudgetReservation[] = []
  override reserve(
    groupId: string,
    request: GroupBudgetRequest & { memberId: string },
  ): GroupBudgetReservation {
    const reservation = super.reserve(groupId, request)
    this.recorded.push(reservation)
    return reservation
  }
}

describe("nested vehicle group budget (SubAgentOrchestrator child)", () => {
  it("joins the parent's group without re-reserving the token axis, keeping the child's first-turn tools", async () => {
    const noopTool = tool("noop", "does nothing", { type: "object", properties: {} }, () => "ok")
    const store = new RecordingStore()
    const group: RunGroup = { id: "nested", budgetStore: store }

    // (1) The parent holds a FULL token reservation and never settles it — the exact condition that
    // squeezed a re-reserving child to a zero-token grant.
    const parentScope = await GroupBudgetScope.open(
      group,
      { sessionId: "parent" },
      { limits: { tokens: 100_000 }, requested: { tokens: 100_000 } },
    )
    expect(parentScope.granted).toEqual({ tokens: 100_000 })

    // (2) Parent runner: shares the group + the noop-bearing plane with its spawned child.
    const provider = new RecordingProvider()
    const plane = new LocalExecutionPlane()
    plane.register(noopTool)
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: plane,
      maxTokens: 4096,
      maxTotalTokens: 100_000,
      runGroup: group,
      agentId: "parent",
    })

    // Inject an active parent kernel (spawnSubAgent requires a live parent run). Mount `noop` as a
    // capability so the kernel-computed spawn manifest can carry it through the capability filter —
    // set_tools alone populates sm.tools, not the ctx.capabilities the spawn manifest reads.
    const runtime = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    await durableStartKernelV2(runtime, sessionLog, "parent")
    await durableStepKernelV2(
      runtime,
      sessionLog,
      "parent",
      capabilityCommandMount(capabilityTool(noopTool.schema)),
    )
    ;(runner as never as { activeKernel: unknown }).activeKernel = runtime
    ;(runner as never as { currentSessionId: string }).currentSessionId = "parent"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    // (3) Spawn the child through the full kernel path. `capabilityFilter.allowedIds` gates the
    // kernel spawn manifest's permitted_capability_ids to just `noop` (empty allow-list ⇒ deny-all).
    const spec: AgentRunSpec = {
      identity: { agentId: "worker", sessionId: "worker-child", isSubAgent: true },
      role: "implement",
      isolation: "shared",
      goal: "do the work",
      capabilityFilter: { allowedIds: ["noop"] },
    }
    const events: StreamEvent[] = []
    for await (const event of runner.spawnSubAgent(spec)) events.push(event)

    // (a) The child completed cleanly — no error event, a terminal done with status "completed".
    expect(events.some(e => (e as { type: string }).type === "error")).toBe(false)
    const done = events.find(e => (e as { type: string }).type === "done") as
      | { type: "done"; status: string }
      | undefined
    expect(done).toBeDefined()
    expect(done!.status).toBe("completed")

    // (b) The child's FIRST LLM call still saw `noop` — the thing the zero-token grant used to strip.
    expect(provider.calls.length).toBeGreaterThan(0)
    expect(provider.calls[0]).toContain("noop")

    // (c) The child's reservation reserved NO axis: granted deep-equals `{}` (no tokens/subagents/rounds).
    const childReservation = store.recorded.find(r => r.memberId === "worker-child")
    expect(childReservation).toBeDefined()
    expect(childReservation!.granted).toEqual({})

    // (d) The parent's full reservation is still held (never settled/released): a fresh token request
    // is squeezed to 0 because the parent still occupies the whole 100_000 in the ledger.
    expect(parentScope.isClosed).toBe(false)
    const probe = await GroupBudgetScope.open(
      group,
      { sessionId: "probe" },
      { limits: { tokens: 100_000 }, requested: { tokens: 100_000 } },
    )
    expect(probe.granted.tokens).toBe(0)
    await probe.release()
  })
})
