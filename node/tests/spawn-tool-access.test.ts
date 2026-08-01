/** Workflow-node capability filtering must preserve intentional quarantine deny-all semantics. */
import { jest } from "@jest/globals"
import {
  InMemorySessionLog,
  LocalExecutionPlane,
  type StreamEvent,
} from "../src/index.js"
import type { LLMProvider, Message, RenderedContext, ToolSchema } from "../src/types.js"
import { tool } from "../src/tools/index.js"
import { defaultSubAgentOrchestrator, type SubAgentRunContext } from "../src/runtime/sub-agent-orchestrator.js"
import type { RuntimeOptions } from "../src/runtime/runner.js"

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

function makeParentOptions(): RuntimeOptions {
  const noopTool = tool("noop", "does nothing", { type: "object", properties: {} }, () => "ok")
  const provider = new RecordingProvider()
  const plane = new LocalExecutionPlane()
  plane.register(noopTool)
  return {
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: plane,
    maxTokens: 4096,
    maxTotalTokens: 100_000,
    agentId: "parent",
  }
}

describe("workflow-node tool access", () => {
  it("does not warn when a quarantined workflow node intentionally resolves to zero tools", async () => {
    // Drive the orchestrator directly with `isWorkflowNode: true` (no full workflow DAG needed): a
    // quarantined node runs filtered with no grants by design, so the misconfig warning must NOT fire.
    const parentOpts = makeParentOptions()
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
