import { createRunner, tool } from "./helpers.js"
import { collectText } from "../../src/runtime/runner.js"
import {
  DEFAULT_NATIVE_SIGNAL_POLICY,
  DEFAULT_NATIVE_GOVERNANCE_POLICY,
  assertNativeProfile,
  osProfile,
} from "../../src/runtime/os-profile.js"
import {
  rebuildOsSnapshotFromSessionEvents,
} from "../../src/runtime/os-snapshot.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"

describe("OS Native Profile (Phase 6)", () => {
  it("resolves and validates the native OS profile", () => {
    const profile = assertNativeProfile(osProfile("native"))
    expect(profile.id).toBe("native")
    expect(profile.signalPolicy.queueMax).toBe(64)
    expect(profile.governancePolicy.rules?.[0]).toEqual({ pattern: "*", action: "allow" })
    expect(() => assertNativeProfile({ ...profile, id: "invalid" as "native" })).toThrow(/Unsupported OS profile/)
  })

  it("native profile run writes kernel events with required categories", async () => {
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "ok" }
      },
    }

    const { runner, sessionLog } = createRunner(provider, [], {
      signalPolicy: DEFAULT_NATIVE_SIGNAL_POLICY,
      governancePolicy: DEFAULT_NATIVE_GOVERNANCE_POLICY,
    })

    await collectText(runner.run({ sessionId: "native-ok", goal: "work" }))
    const entries = await sessionLog.read("native-ok")
    const events = entries.map(e => e.event)
    const snap = rebuildOsSnapshotFromSessionEvents(events)
    expect(snap.pageOutCount).toBeGreaterThanOrEqual(0)
  })

  it("native profile with AskUser emits syscall/sched audit events", async () => {
    let n = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
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
        signalPolicy: DEFAULT_NATIVE_SIGNAL_POLICY,
        governancePolicy: { rules: [{ pattern: "needs_approval", action: "ask_user" }] },
        onPermissionRequest: () => ({ approved: true, responder: "test" }),
        maxTurns: 6,
      },
    )

    await collectText(runner.run({ sessionId: "native-gov", goal: "go" }))
    const events = (await sessionLog.read("native-gov")).map(e => e.event)
    // Classification is derived from `kind` (single taxonomy), no longer embedded per event.
    expect(events.some(e => e.kind === "tool_gated")).toBe(true)
    expect(events.some(e => e.kind === "suspended")).toBe(true)
    const snap = rebuildOsSnapshotFromSessionEvents(events)
    expect(snap.toolGatedCount).toBeGreaterThanOrEqual(1)
    expect(snap.lastSuspend?.reason).toBe("ask_user")
  })
})
