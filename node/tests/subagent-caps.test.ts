/**
 * O3 — per-child spawn caps: `AgentRunSpec.maxTurns` / `maxWallMs` bound ONE sub-agent's run
 * independently of the parent's limits (the Claude Code per-subagent maxTurns/budget pattern).
 * The child terminates with an attributable reason (`max_turns` / `timeout`) the parent can read
 * off `SubAgentResult.result.termination` to decide retry / skip / abort.
 *
 * Drives the orchestrator directly with a manifest granting the tool, so the child actually
 * EXECUTES calls each turn (turns only advance on tool results; a denied call rolls back
 * without advancing — that path is the repeat fuse's territory, tested in the kernel).
 */
import { defaultSubAgentOrchestrator } from "../src/runtime/sub-agent-orchestrator.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import { agentIdentitySub, type AgentRunSpec } from "../src/types/agent.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import type { RuntimeOptions } from "../src/runtime/runner.js"

/** Never stops calling tools — only an external cap can end its run. Args vary per call so the
 *  kernel repeat fuse (O6) reads it as real iteration, not a stall. */
class LoopingProvider implements LLMProvider {
  calls = 0
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }
  async *stream(): AsyncIterable<StreamEvent> {
    this.calls += 1
    yield { type: "tool_call", id: `call_${this.calls}`, name: "ping", arguments: { n: this.calls } }
  }
}

function parentOpts(provider: LLMProvider): RuntimeOptions {
  const plane = new LocalExecutionPlane()
  plane.register(tool("ping", "Ping", {
    type: "object",
    properties: { n: { type: "number" } },
  }, () => "pong"))
  return {
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: plane,
    maxTokens: 4096,
    maxTurns: 25, // generous parent cap — the child's own cap must win
  } as RuntimeOptions
}

function runChild(opts: RuntimeOptions, parentSessionId: string, spec: AgentRunSpec) {
  return defaultSubAgentOrchestrator.run({
    parentOpts: opts,
    parentSessionId,
    spec,
    manifest: {
      kind: "agent_process_changed",
      agent_id: spec.identity.agentId,
      parent_session_id: parentSessionId,
      role: spec.role,
      isolation: spec.isolation ?? "shared",
      context_inheritance: "none",
      permitted_capability_ids: ["ping"],
    },
    sessionLog: opts.sessionLog,
  })
}

describe("per-subagent caps (AgentRunSpec.maxTurns / maxWallMs)", () => {
  it("caps the child at its own maxTurns and reports max_turns to the parent", async () => {
    const provider = new LoopingProvider()
    const result = await runChild(parentOpts(provider), "parent-1", {
      identity: agentIdentitySub("child-a", "parent-1-child-a", "parent-1"),
      role: "explore",
      goal: "loop forever",
      maxTurns: 2,
    })
    expect(result.result.termination).toBe("max_turns")
    // 2 turns + the budget-exceeded final report call; the parent's 25 must not apply.
    expect(provider.calls).toBeLessThanOrEqual(3)
  })

  it("falls back to the parent's maxTurns when the spec sets none", async () => {
    const provider = new LoopingProvider()
    const opts = parentOpts(provider)
    opts.maxTurns = 3
    const result = await runChild(opts, "parent-2", {
      identity: agentIdentitySub("child-b", "parent-2-child-b", "parent-2"),
      role: "explore",
      goal: "loop forever",
    })
    expect(result.result.termination).toBe("max_turns")
    expect(provider.calls).toBeLessThanOrEqual(4)
  })
})
