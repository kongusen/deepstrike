import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import {
  GroupBudgetScope,
  InMemoryGroupBudgetStore,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  type RunGroup,
} from "../src/index.js"
import { tool } from "../src/tools/index.js"

class ToolThenTextProvider implements LLMProvider {
  private turn = 0
  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }
  async *stream(_ctx: RenderedContext, _tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    this.turn += 1
    if (this.turn === 1) yield { type: "tool_call", id: "call_1", name: "noop", arguments: {} }
    else yield { type: "text_delta", delta: "done" }
  }
}

const noopTool = tool("noop", "does nothing", { type: "object", properties: {} }, () => "ok")

function makeRunner(
  runGroup?: RunGroup,
  agentId?: string,
  kernelReliability?: { hostEffectRetryAttempts: number },
): RuntimeRunner {
  const plane = new LocalExecutionPlane()
  plane.register(noopTool)
  return new RuntimeRunner({
    provider: new ToolThenTextProvider(),
    sessionLog: new InMemorySessionLog(),
    executionPlane: plane,
    maxTokens: 4096,
    maxTotalTokens: 100_000,
    agentId,
    runGroup,
    kernelReliability,
  })
}

async function runToDone(runner: RuntimeRunner, sessionId: string): Promise<{ status: string; totalTokens: number }> {
  let done = { status: "", totalTokens: 0 }
  for await (const event of runner.run({ sessionId, goal: "open the scene" })) {
    if ((event as { type: string }).type === "done") {
      const terminal = event as unknown as { status: string; totalTokens: number }
      done = { status: terminal.status, totalTokens: terminal.totalTokens }
    }
  }
  return done
}

describe("RunGroup reservation-backed budgets", () => {
  it("atomically reserves capacity without overselling", async () => {
    const store = new InMemoryGroupBudgetStore()
    const group: RunGroup = { id: "concurrent", budgetStore: store }
    const request = { limits: { tokens: 100 }, requested: { tokens: 100 } }
    const [first, second] = await Promise.all([
      GroupBudgetScope.open(group, { sessionId: "a" }, request),
      GroupBudgetScope.open(group, { sessionId: "b" }, request),
    ])

    expect(first.granted).toEqual({ tokens: 100 })
    expect(second.granted).toEqual({ tokens: 0 })
    await first.settle({ tokens: 60 })
    await second.release()
    expect(store.read(group.id).tokensSpent).toBe(60)
  })

  it("does not turn an unrequested axis into a zero grant", async () => {
    const store = new InMemoryGroupBudgetStore()
    const scope = await GroupBudgetScope.open(
      { id: "partial", budgetStore: store },
      { sessionId: "a" },
      { limits: { subagents: 4 }, requested: { subagents: 2 } },
    )

    expect(scope.granted).toEqual({ subagents: 2 })
    await scope.release()
  })

  it("keeps a reservation open when settlement fails so the same report can retry", async () => {
    class FlakyStore extends InMemoryGroupBudgetStore {
      attempts = 0
      override settle(groupId: string, reservationId: string, actual: { tokens?: number }): void {
        this.attempts += 1
        if (this.attempts === 1) throw new Error("temporary store failure")
        super.settle(groupId, reservationId, actual)
      }
    }
    const store = new FlakyStore()
    const scope = await GroupBudgetScope.open(
      { id: "retry", budgetStore: store },
      { sessionId: "a" },
      { limits: { tokens: 100 }, requested: { tokens: 100 } },
    )

    await expect(scope.settle({ tokens: 40 })).rejects.toThrow("temporary store failure")
    expect(scope.isClosed).toBe(false)
    await scope.settle({ tokens: 40 })
    expect(store.read("retry").tokensSpent).toBe(40)
  })

  it("retries terminal settlement according to the SDK reliability policy", async () => {
    class FlakyStore extends InMemoryGroupBudgetStore {
      attempts = 0
      override settle(groupId: string, reservationId: string, actual: { tokens?: number }): void {
        this.attempts += 1
        if (this.attempts === 1) throw new Error("temporary store failure")
        super.settle(groupId, reservationId, actual)
      }
    }
    const store = new FlakyStore()
    const runner = makeRunner(
      { id: "host-retry", budgetStore: store },
      undefined,
      { hostEffectRetryAttempts: 1 },
    )

    expect((await runToDone(runner, "member")).status).toBe("completed")
    expect(store.attempts).toBe(2)
  })

  it("enforces an exhausted group through a zero-capacity reservation", async () => {
    const store = new InMemoryGroupBudgetStore()
    const group: RunGroup = { id: "exhausted", budgetStore: store }
    const seed = await GroupBudgetScope.open(
      group,
      { sessionId: "prior-member" },
      { limits: { tokens: 100_000 }, requested: { tokens: 100_000 } },
    )
    await seed.settle({ tokens: 100_000 })
    expect((await runToDone(makeRunner(group), "director")).status).toBe("token_budget")
  })

  it("settles kernel-reported local usage and preserves member lineage", async () => {
    const store = new InMemoryGroupBudgetStore()
    const group: RunGroup = { id: "usage", budgetStore: store }
    const first = await runToDone(makeRunner(group, "director"), "director")
    const second = await runToDone(makeRunner(group, "critic"), "critic")

    expect(first.status).toBe("completed")
    expect(second.status).toBe("completed")
    expect(store.read(group.id).tokensSpent).toBe(first.totalTokens + second.totalTokens)
    expect((await store.members(group.id)).map(member => member.sessionId).sort()).toEqual(["critic", "director"])
  })

  it("keeps the same run per-vehicle when no group is configured", async () => {
    expect((await runToDone(makeRunner(), "solo")).status).toBe("completed")
  })
})
