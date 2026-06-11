/**
 * Unit test for `RuntimeRunner.runWorkflow` against the kernel's W2-1 run-queue executor.
 *
 * The run-queue unblocks a node's dependents the moment *that* node completes (per-node unblock),
 * so a single `sub_agent_completed` feed can emit its own `workflow_batch_spawned`. The drive loop
 * must ACCUMULATE the nodes spawned across every feed in a round — the previous loop kept only the
 * last feed's batch and dropped nodes unblocked by earlier completions, stalling uneven DAGs.
 *
 * Uses a fully scripted fake kernel (no native core / no provider): the script reproduces the exact
 * observation sequence the Rust kernel test `workflow_run_queue_unblocks_dependents_per_node` proves
 * the kernel emits for the diamond DAG  A,B → C  and  A → D.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { WorkflowSpec } from "../src/index.js"

type Obs = { kind: string; nodes?: unknown[]; completed?: string[]; failed?: string[] }

function node(agent_id: string, goal: string) {
  return {
    agent_id,
    goal,
    role: "implement",
    isolation: "shared",
    context_inheritance: "none",
    model_hint: null,
    trust: "trusted",
  }
}

/** Scripted kernel: maps each event to the run-queue observations for the diamond DAG. */
function makeFakeKernel() {
  const A = node("wf-node0", "A")
  const B = node("wf-node1", "B")
  const C = node("wf-node2", "C") // depends on A & B
  const D = node("wf-node3", "D") // depends on A only

  function reply(obs: Obs[]): string {
    return JSON.stringify({ version: 1, actions: [], observations: obs })
  }

  return {
    turn: () => 0,
    step(input: string): string {
      const { event } = JSON.parse(input) as { event: { kind: string; result?: { agent_id: string } } }
      if (event.kind === "load_workflow") {
        // A and B have no deps → both ready in the first round.
        return reply([{ kind: "workflow_batch_spawned", nodes: [A, B] }])
      }
      if (event.kind === "sub_agent_completed") {
        switch (event.result?.agent_id) {
          // A done (B still running) → D unblocks immediately (per-node unblock).
          case "wf-node0":
            return reply([{ kind: "workflow_batch_spawned", nodes: [D] }])
          // B done → C unblocks (both deps satisfied).
          case "wf-node1":
            return reply([{ kind: "workflow_batch_spawned", nodes: [C] }])
          // D done → nothing new.
          case "wf-node3":
            return reply([])
          // C done → DAG complete.
          case "wf-node2":
            return reply([
              { kind: "workflow_completed", completed: ["wf-node0", "wf-node1", "wf-node2", "wf-node3"], failed: [] },
            ])
        }
      }
      return reply([])
    },
  }
}

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

    // Wire the scripted fake kernel as the active parent run (runWorkflow runs mid-run).
    ;(runner as never as { activeKernel: unknown }).activeKernel = makeFakeKernel()
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-rq"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    const spec: WorkflowSpec = {
      nodes: [
        { task: "A", role: "implement" },
        { task: "B", role: "implement" },
        { task: "C", role: "implement", dependsOn: [0, 1] },
        { task: "D", role: "implement", dependsOn: [0] },
      ],
    }

    const outcome = await runner.runWorkflow(spec)

    // The critical assertion: D (unblocked by A alone) is NOT dropped — all four nodes ran.
    expect(ran.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2", "wf-node3"])
    expect(outcome.completed.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2", "wf-node3"])
    expect(outcome.failed).toEqual([])
  })
})
