import { RuntimeRunner, collectText, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import { tool } from "../src/tools/index.js"
import {
  DEFAULT_NATIVE_ATTENTION_POLICY,
  DEFAULT_NATIVE_GOVERNANCE_POLICY,
} from "../src/runtime/os-profile.js"
import {
  rebuildOsSnapshotFromSessionEvents,
  sessionLogHasRequiredCategories,
} from "../src/runtime/os-snapshot.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

function createRunner(
  provider: LLMProvider,
  tools: ReturnType<typeof tool>[] = [],
  opts: {
    osProfile?: "legacy" | "native"
    governancePolicy?: typeof DEFAULT_NATIVE_GOVERNANCE_POLICY
    attentionPolicy?: { maxQueueSize?: number }
    onPermissionRequest?: (req: { type: string; callId: string; toolName: string }) => Promise<{ approved: boolean; responder: string }>
    maxTurns?: number
  } = {},
) {
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  for (const t of tools) plane.register(t)
  const runner = new RuntimeRunner({
    provider,
    sessionLog,
    executionPlane: plane,
    maxTokens: 2048,
    maxTurns: opts.maxTurns ?? 25,
    osProfile: opts.osProfile,
    governancePolicy: opts.governancePolicy,
    attentionPolicy: opts.attentionPolicy,
    onPermissionRequest: opts.onPermissionRequest,
  })
  return { runner, sessionLog }
}

describe("OS Native Profile (Phase 6)", () => {
  it("fail-fast when native profile missing attentionPolicy", () => {
    const provider: LLMProvider = {
      async complete(): Promise<Message> { return { role: "assistant", content: "x", toolCalls: [] } },
      async *stream(): AsyncIterable<StreamEvent> { yield { type: "text_delta", delta: "x" } },
    }
    const { runner } = createRunner(provider, [], {
      osProfile: "native",
      governancePolicy: DEFAULT_NATIVE_GOVERNANCE_POLICY,
    })
    return expect(collectText(runner.run({ sessionId: "native-fail-att", goal: "g" }))).rejects.toThrow(
      /attentionPolicy/,
    )
  })

  it("native profile run writes kernel events with required categories", async () => {
    const provider: LLMProvider = {
      async complete(): Promise<Message> { return { role: "assistant", content: "done", toolCalls: [] } },
      async *stream(): AsyncIterable<StreamEvent> { yield { type: "text_delta", delta: "ok" } },
    }
    const { runner, sessionLog } = createRunner(provider, [], {
      osProfile: "native",
      attentionPolicy: DEFAULT_NATIVE_ATTENTION_POLICY,
      governancePolicy: DEFAULT_NATIVE_GOVERNANCE_POLICY,
    })
    await collectText(runner.run({ sessionId: "native-ok", goal: "work" }))
    const events = (await sessionLog.read("native-ok")).map(e => e.event)
    expect(sessionLogHasRequiredCategories(events)).toBe(true)
    expect(rebuildOsSnapshotFromSessionEvents(events).pageOutCount).toBeGreaterThanOrEqual(0)
  })

  it("native profile with AskUser emits syscall/sched audit events", async () => {
    let n = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> { return { role: "assistant", content: "", toolCalls: [] } },
      async *stream(): AsyncIterable<StreamEvent> {
        n += 1
        if (n === 1) {
          yield { type: "tool_call", id: "c1", name: "needs_approval", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }
    const { runner, sessionLog } = createRunner(
      provider,
      [tool("needs_approval", "Needs approval", { type: "object", properties: {} }, () => "ok")],
      {
        osProfile: "native",
        attentionPolicy: DEFAULT_NATIVE_ATTENTION_POLICY,
        governancePolicy: { rules: [{ pattern: "needs_approval", action: "ask_user" }] },
        onPermissionRequest: async () => ({ approved: true, responder: "test" }),
        maxTurns: 6,
      },
    )
    await collectText(runner.run({ sessionId: "native-gov", goal: "go" }))
    const events = (await sessionLog.read("native-gov")).map(e => e.event)
    expect(events.some(e => e.kind === "tool_gated" && e.category === "syscall")).toBe(true)
    expect(events.some(e => e.kind === "suspended" && e.category === "sched")).toBe(true)
    expect(sessionLogHasRequiredCategories(events)).toBe(true)
    expect(rebuildOsSnapshotFromSessionEvents(events).toolGatedCount).toBeGreaterThanOrEqual(1)
  })
})
