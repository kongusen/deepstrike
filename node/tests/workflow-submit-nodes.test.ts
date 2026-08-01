import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"

describe("canonical workflow-node submission", () => {
  it("keeps the child completion and audits a parent-request rejection independently", async () => {
    const goals: string[] = []
    const orchestrator = {
      async run(ctx: { spec: { goal: string }; manifest: { agent_id: string } }) {
        goals.push(ctx.spec.goal)
        return {
          agentId: ctx.manifest.agent_id,
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: "submitted", toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
          submittedNodes: [{ task: "discovered", role: "implement" }],
        }
      },
    }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      sessionLog,
      maxTokens: 8000,
      resourceQuota: { maxWorkflowNodes: 1 },
      subAgentOrchestrator: orchestrator as never,
    } as never)

    const outcome = await runner.runWorkflow(
      { nodes: [{ task: "root", role: "implement" }] },
      { sessionId: "wf-parent-request-denied" },
    )

    expect(goals).toEqual([expect.stringContaining("root")])
    expect(outcome.nodeOutcomes).toEqual([
      expect.objectContaining({ nodeId: "wf-node0", status: "completed", termination: "completed" }),
    ])
    expect(outcome.outputs["wf-node0"]).toBe("submitted")
    const events = await sessionLog.read("wf-parent-request-denied")
    expect(events).toContainEqual(expect.objectContaining({
      event: expect.objectContaining({
        kind: "kernel_observation",
        observation_kind: "control_request_rejected",
        raw: expect.objectContaining({
          operation: "submit_workflow_nodes",
          subject: "wf-node0",
        }),
      }),
    }))
  })
})
