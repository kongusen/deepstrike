/**
 * Unit test for `RuntimeRunner.runWorkflow` against the kernel's W2-1 run-queue executor.
 *
 * The run-queue unblocks a node's dependents the moment *that* node completes (per-node unblock),
 * so a single `sub_agent_completed` feed can emit its own `workflow_batch_spawned`. The drive loop
 * must ACCUMULATE the nodes spawned across every feed in a round — the previous loop kept only the
 * last feed's batch and dropped nodes unblocked by earlier completions, stalling uneven DAGs.
 *
 * The canonical workflow root now exercises the real journaled run queue for the diamond DAG
 * A,B → C and A → D.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { WorkflowSpec } from "../src/index.js"

describe("runWorkflow over the run-queue executor", () => {
  it("runs every node of an uneven DAG, including a dependent unblocked by a single early completion", async () => {
    const ran: string[] = []
    const mockOrchestrator = {
      // Records each node it is asked to run and returns a canned completion for it.
      async run(ctx: { manifest: { agent_id: string } }) {
        const agentId = ctx.manifest.agent_id
        ran.push(agentId)
        return {
          agentId,
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: "ok", toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      },
    }

    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: mockOrchestrator as never,
    } as never)

    const spec: WorkflowSpec = {
      nodes: [
        { task: "A", role: "implement" },
        { task: "B", role: "implement" },
        { task: "C", role: "implement", dependsOn: [0, 1] },
        { task: "D", role: "implement", dependsOn: [0] },
      ],
    }

    const outcome = await runner.runWorkflow(spec, { sessionId: "wf-rq" })

    // The critical assertion: D (unblocked by A alone) is NOT dropped — all four nodes ran.
    expect(ran.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2", "wf-node3"])
    expect(outcome.nodeOutcomes.map(node => node.nodeId).sort()).toEqual(["wf-node0", "wf-node1", "wf-node2", "wf-node3"])
    expect(outcome.nodeOutcomes.every(node => node.status === "completed")).toBe(true)
  })
})
