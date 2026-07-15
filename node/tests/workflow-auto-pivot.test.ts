/**
 * M5 v2.1: top-level auto-pivot. When a TOP-LEVEL agent (not a workflow node) calls the
 * `start_workflow` tool mid-conversation, the runner records the authored spec, drives it in its own
 * kernel at the safe point (after the tool turn resolves → kernel back in Reason, not suspended), then
 * injects the workflow outcome into context and resumes the reason loop. Pure SDK — no kernel change.
 */
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import { startWorkflowTool } from "../src/types/agent.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

/** Emits a `start_workflow` tool call on turn 1, then plain text (terminates) afterwards. */
class AuthoringProvider implements LLMProvider {
  turn = 0
  readonly contexts: RenderedContext[] = []
  async complete(): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  }
  async *stream(context: RenderedContext, _tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    this.contexts.push(context)
    this.turn += 1
    if (this.turn === 1) {
      yield {
        type: "tool_call",
        id: "call-1",
        name: "start_workflow",
        arguments: { spec: { nodes: [
          { task: "explore A", role: "implement" },
          { task: "explore B", role: "implement" },
        ] } },
      } as unknown as StreamEvent
    } else {
      yield { type: "text_delta", delta: "synthesized the sub-workflow results" } as StreamEvent
    }
  }
}

describe("M5 v2.1 top-level start_workflow auto-pivot", () => {
  it("drives the authored sub-workflow in-kernel and resumes the agent with the outcome", async () => {
    const ran: string[] = []
    // Mock workflow driver: each authored node returns a canned completion (no real LLM).
    const orchestrator = {
      async run(ctx: { spec: { identity: { agentId: string } } }) {
        const agentId = ctx.spec.identity.agentId
        ran.push(agentId)
        return {
          agentId,
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: `result of ${agentId}`, toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      },
    }

    const provider = new AuthoringProvider()
    const plane = new LocalExecutionPlane()
    // Register start_workflow so it's offered to the model; its execute never runs (intercepted).
    plane.register(tool(startWorkflowTool.name, startWorkflowTool.description, JSON.parse(startWorkflowTool.parameters), async () => ""))

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 8000,
      maxTurns: 5,
      subAgentOrchestrator: orchestrator as never,
      // NOTE: isWorkflowNode unset ⇒ this is a top-level run ⇒ start_workflow auto-pivots.
    } as never)

    let text = ""
    for await (const evt of runner.run({ sessionId: "auto-pivot", goal: "explore the topic two ways then synthesize" })) {
      if (evt.type === "text_delta") text += (evt as { delta: string }).delta
    }

    // The authored sub-workflow ran both nodes in this kernel (no separate child kernel).
    expect(ran.sort()).toEqual(["wf-node0", "wf-node1"])
    // The agent got a 2nd turn AFTER the workflow, and its context carried the injected outcome.
    expect(provider.contexts.length).toBeGreaterThanOrEqual(2)
    const secondCtx = provider.contexts[1]
    const allContent = [
      secondCtx.systemText, secondCtx.systemStable, secondCtx.systemKnowledge,
      secondCtx.stateTurn?.content, ...secondCtx.turns.map(m => m.content),
    ].filter(Boolean).join("\n")
    // The kernel-owned continuation already carries each node result in task state; the SDK must
    // not synthesize a second, unbudgeted provider context merely to add a redundant wrapper note.
    expect(allContent).not.toContain("[authored workflow result]")
    expect(allContent).toContain("result of wf-node0")
    // The run continued past the authoring turn and produced the final synthesis text.
    expect(text).toContain("synthesized the sub-workflow results")
  })
})
