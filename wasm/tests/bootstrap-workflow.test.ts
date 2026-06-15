/**
 * M5 v2 / G1 (wasm): `bootstrapWorkflow` routes an agent-authored spec through the agent-reachable
 * `submit_workflow` kernel event (Syscall::LoadWorkflow) and drives the bootstrapped DAG. WASM has no
 * native kernel in tests, so a scripted fake kernel reproduces the observation sequence the Rust
 * kernel test `submit_workflow_bootstraps_dag_when_no_workflow_active` proves the kernel emits.
 */
import { RuntimeRunner, InMemorySessionLog, submitWorkflowToKernel } from "../src/index.js"
import type { WorkflowSpec } from "../src/index.js"

describe("submitWorkflowToKernel (wasm)", () => {
  it("lowers a spec to the submit_workflow event with the parent session id", () => {
    const ev = submitWorkflowToKernel({ nodes: [{ task: "x", role: "implement" }] }, "sess-1")
    expect(ev.kind).toBe("submit_workflow")
    expect(ev.parent_session_id).toBe("sess-1")
    expect((ev.spec as { nodes: unknown[] }).nodes).toHaveLength(1)
    expect(ev.submitter_agent_id).toBeUndefined()
    expect(submitWorkflowToKernel({ nodes: [] }, "s", "wf-node3").submitter_agent_id).toBe("wf-node3")
  })
})

type Obs = { kind: string; nodes?: unknown[]; completed?: string[]; failed?: string[] }
const node = (agent_id: string, goal: string) => ({
  agent_id, goal, role: "implement", isolation: "shared", context_inheritance: "none", model_hint: null, trust: "trusted",
})

/** Scripted kernel: a top-level agent authors a 2-node spec via submit_workflow → bootstrap batch. */
function makeFakeKernel() {
  const A = node("wf-node0", "A")
  const B = node("wf-node1", "B")
  const reply = (obs: Obs[]) => JSON.stringify({ version: 1, actions: [], observations: obs })
  return {
    turn: () => 0,
    step(input: string): string {
      const { event } = JSON.parse(input) as { event: { kind: string; result?: { agent_id: string } } }
      // The agent-reachable bootstrap event — both A and B are independent → first batch.
      if (event.kind === "submit_workflow") return reply([{ kind: "workflow_batch_spawned", nodes: [A, B] }])
      if (event.kind === "sub_agent_completed") {
        if (event.result?.agent_id === "wf-node1") {
          return reply([{ kind: "workflow_completed", completed: ["wf-node0", "wf-node1"], failed: [] }])
        }
        return reply([])
      }
      return reply([])
    },
  }
}

describe("bootstrapWorkflow drives an agent-authored DAG (wasm)", () => {
  it("bootstraps via submit_workflow and runs every authored node", async () => {
    const ran: string[] = []
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string } }) {
        ran.push(ctx.manifest.agent_id)
        return {
          agentId: ctx.manifest.agent_id,
          result: { termination: "completed", finalMessage: { role: "assistant", content: "ok", toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 },
        }
      },
    }
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: orchestrator as never,
    } as never)
    ;(runner as never as { activeKernel: unknown }).activeKernel = makeFakeKernel()
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-boot"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    const spec: WorkflowSpec = { nodes: [{ task: "A", role: "implement" }, { task: "B", role: "implement" }] }
    const outcome = await runner.bootstrapWorkflow(spec)

    expect(ran.sort()).toEqual(["wf-node0", "wf-node1"])
    expect(outcome.completed.sort()).toEqual(["wf-node0", "wf-node1"])
    expect(outcome.failed).toEqual([])
    // M5 v2.1: outputs are threaded out of the driver (the auto-pivot injects them into the agent's
    // context). Each completed node's output is keyed by agent id.
    expect(outcome.outputs["wf-node0"]).toBe("ok")
    expect(outcome.outputs["wf-node1"]).toBe("ok")
  })
})
