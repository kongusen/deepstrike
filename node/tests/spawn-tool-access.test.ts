/**
 * `AgentRunSpec.toolAccess` on the public spawn path (`RuntimeRunner.spawnSubAgent`).
 *
 * Before this field the spawn path never set the orchestrator's `toolAccess`, so every spawned
 * sub-agent ran `"filtered"`; with no capability mounted the filter resolved to deny-all and the
 * child model saw "no tools available". Two cases pin the fix:
 *
 *  (a) `toolAccess:"inherit"` with NO capability mounting — the child runs on the parent's execution
 *      plane, so its first provider call still carries the parent's `noop` tool and it completes.
 *  (b) the default (`"filtered"`) with no capability — the child resolves to zero tools; the
 *      orchestrator emits a host-visible `console.warn` ("zero tools"), and the child still runs to
 *      completion (the warning is advisory, not fatal).
 *
 * The parent kernel is injected (mirrors spawn-sub-agent-deny.test.ts / nested-group-vehicle.test.ts);
 * a RecordingProvider captures the tool names handed to each LLM call.
 */
import { jest } from "@jest/globals"
import { getKernel } from "../src/kernel.js"
import {
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  type AgentRunSpec,
  type StreamEvent,
} from "../src/index.js"
import type { LLMProvider, Message, RenderedContext, ToolSchema } from "../src/types.js"
import { tool } from "../src/tools/index.js"
import { defaultSubAgentOrchestrator, type SubAgentRunContext } from "../src/runtime/sub-agent-orchestrator.js"
import type { RuntimeOptions } from "../src/runtime/runner.js"
import { durableStartKernelV2 } from "./helpers/kernel-v2.js"

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

/** Parent runner over a `noop`-bearing plane with an injected, already-started kernel (spawnSubAgent
 *  requires a live parent run). No capability is mounted — the two cases exercise the un-granted path. */
async function makeParent(): Promise<{ runner: RuntimeRunner; provider: RecordingProvider }> {
  const noopTool = tool("noop", "does nothing", { type: "object", properties: {} }, () => "ok")
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
    agentId: "parent",
  })
  const runtime = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
  await durableStartKernelV2(runtime, sessionLog, "parent")
  ;(runner as never as { activeKernel: unknown }).activeKernel = runtime
  ;(runner as never as { currentSessionId: string }).currentSessionId = "parent"
  ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []
  return { runner, provider }
}

describe("spawnSubAgent tool access (AgentRunSpec.toolAccess)", () => {
  it("(a) toolAccess:'inherit' runs the child on the parent's plane — first-turn tools survive without a capability grant", async () => {
    const { runner, provider } = await makeParent()

    const spec: AgentRunSpec = {
      identity: { agentId: "worker", sessionId: "worker-inherit", isSubAgent: true },
      role: "implement",
      isolation: "shared",
      goal: "do the work",
      toolAccess: "inherit",
    }
    const events: StreamEvent[] = []
    for await (const event of runner.spawnSubAgent(spec)) events.push(event)

    // The child completed cleanly and its FIRST LLM call still saw the parent-plane `noop`.
    expect(events.some(e => (e as { type: string }).type === "error")).toBe(false)
    const done = events.find(e => (e as { type: string }).type === "done") as
      | { type: "done"; status: string }
      | undefined
    expect(done?.status).toBe("completed")
    expect(provider.calls.length).toBeGreaterThan(0)
    expect(provider.calls[0]).toContain("noop")
  })

  it("(b) default 'filtered' with no capability resolves to zero tools — warns the host but still completes", async () => {
    const { runner } = await makeParent()
    const warnSpy = jest.spyOn(console, "warn").mockImplementation(() => {})
    try {
      const spec: AgentRunSpec = {
        identity: { agentId: "worker", sessionId: "worker-filtered", isSubAgent: true },
        role: "implement",
        isolation: "shared",
        goal: "do the work",
        // toolAccess omitted ⇒ default "filtered"; no capabilityFilter ⇒ deny-all.
      }
      const events: StreamEvent[] = []
      for await (const event of runner.spawnSubAgent(spec)) events.push(event)

      // The zero-tool warning fired, and it is advisory: the child still ran to a clean completion.
      expect(warnSpy).toHaveBeenCalled()
      const warned = warnSpy.mock.calls.map(c => String(c[0])).join("\n")
      expect(warned).toContain("zero tools")
      expect(warned).toContain("worker")
      const done = events.find(e => (e as { type: string }).type === "done") as
        | { type: "done"; status: string }
        | undefined
      expect(done?.status).toBe("completed")
    } finally {
      warnSpy.mockRestore()
    }
  })

  it("(c) a workflow node that resolves to zero filtered tools is EXEMPT — no warning (intentional quarantine deny-all)", async () => {
    // Drive the orchestrator directly with `isWorkflowNode: true` (no full workflow DAG needed): a
    // quarantined node runs filtered with no grants by design, so the misconfig warning must NOT fire.
    const { runner } = await makeParent()
    const parentOpts = (runner as never as { opts: RuntimeOptions }).opts
    const warnSpy = jest.spyOn(console, "warn").mockImplementation(() => {})
    try {
      const ctx: SubAgentRunContext = {
        parentOpts,
        parentSessionId: "parent",
        spec: {
          identity: { agentId: "wf-node", sessionId: "parent-wf-node", isSubAgent: true },
          role: "verify",
          isolation: "read_only",
          goal: "check the untrusted content",
        },
        manifest: {
          kind: "agent_process_changed",
          agent_id: "wf-node",
          parent_session_id: "parent",
          role: "verify",
          isolation: "read_only",
          context_inheritance: "none",
          permitted_capability_ids: [],
        },
        sessionLog: parentOpts.sessionLog,
        isWorkflowNode: true,
        toolAccess: "filtered",
      }
      const result = await defaultSubAgentOrchestrator.run(ctx)
      expect(result.result.termination).toBe("completed")
      const warned = warnSpy.mock.calls.map(c => String(c[0])).join("\n")
      expect(warned).not.toContain("zero tools")
    } finally {
      warnSpy.mockRestore()
    }
  })
})
