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

type Obs = { kind: string; node_outcomes?: Array<{ node_id: string; status: string }> }
const node = (agent_id: string, goal: string) => ({
  agent_id, goal, role: "implement", isolation: "shared", context_inheritance: "none", model_hint: null, trust: "trusted",
})

/** Scripted kernel: a top-level agent authors a 2-node spec via submit_workflow → bootstrap batch. */
function makeFakeKernel() {
  const A = node("wf-node0", "A")
  const B = node("wf-node1", "B")
  const reply = (actions: unknown[], obs: Obs[]) => JSON.stringify({ version: 2, actions, observations: obs, faults: [] })
  return {
    turn: () => 0,
    step(input: string): string {
      const { event } = JSON.parse(input) as { event: { kind: string; result?: { agent_id: string } } }
      // The agent-reachable bootstrap event — both A and B are independent → first batch.
      if (event.kind === "submit_workflow") return reply([{ kind: "spawn_workflow", effect_id: "spawn-1", nodes: [A, B] }], [])
      if (event.kind === "workflow_spawn_result") return reply([], [])
      if (event.kind === "sub_agent_completed") {
        if (event.result?.agent_id === "wf-node1") {
          return reply([{ kind: "call_provider", effect_id: "provider-next", context: {}, tools: [] }], [{ kind: "workflow_completed", node_outcomes: ["wf-node0", "wf-node1"].map(node_id => ({ node_id, status: "completed" })) }])
        }
        return reply([], [])
      }
      return reply([], [])
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
    expect(outcome.nodeOutcomes.map(node => node.nodeId).sort()).toEqual(["wf-node0", "wf-node1"])
    expect(outcome.nodeOutcomes.every(node => node.status === "completed")).toBe(true)
    // M5 v2.1: outputs are threaded out of the driver (the auto-pivot injects them into the agent's
    // context). Each completed node's output is keyed by agent id.
    expect(outcome.outputs["wf-node0"]).toBe("ok")
    expect(outcome.outputs["wf-node1"]).toBe("ok")
  })

  it("keeps the provider continuation when governance rejects the authored workflow", async () => {
    const reply = JSON.stringify({
      version: 2,
      actions: [{ kind: "call_provider", effect_id: "provider-rejected", context: {}, tools: [] }],
      observations: [{
        kind: "control_request_rejected",
        operation: "start_workflow",
        reason: "submit_nodes would grow workflow to 2 nodes (max 1)",
      }],
      faults: [],
    })
    const runner = new RuntimeRunner({ sessionLog: new InMemorySessionLog(), maxTokens: 8000 } as never)
    ;(runner as never as { activeKernel: unknown }).activeKernel = {
      turn: () => 0,
      step: () => reply,
    }
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-rejected"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    await expect(runner.bootstrapWorkflow({ nodes: [
      { task: "A", role: "implement" },
      { task: "B", role: "implement" },
    ] })).resolves.toEqual({
      nodeOutcomes: [],
      outputs: {},
      rejection: {
        operation: "start_workflow",
        reason: "submit_nodes would grow workflow to 2 nodes (max 1)",
      },
    })
  })

  it("returns a typed rejection when a host-loaded workflow is invalid", async () => {
    const reply = JSON.stringify({
      version: 2,
      actions: [{ kind: "call_provider", effect_id: "provider-invalid", context: {}, tools: [] }],
      observations: [{
        kind: "control_request_rejected",
        operation: "load_workflow",
        reason: "workflow node 0 cannot depend on itself",
      }],
      faults: [],
    })
    const runner = new RuntimeRunner({ sessionLog: new InMemorySessionLog(), maxTokens: 8000 } as never)
    ;(runner as never as { activeKernel: unknown }).activeKernel = {
      turn: () => 0,
      step: () => reply,
    }
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-invalid"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    await expect(runner.runWorkflow({ nodes: [
      { task: "self-cycle", role: "implement", dependsOn: [0] },
    ] })).resolves.toEqual({
      nodeOutcomes: [],
      outputs: {},
      rejection: {
        operation: "load_workflow",
        reason: "workflow node 0 cannot depend on itself",
      },
    })
  })
})
