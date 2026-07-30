import { getKernel } from "../src/kernel.js"
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { submitWorkflowNodesToKernel } from "../src/types/agent.js"
import type { WorkflowSpec, WorkflowSpawnInfo } from "../src/types/agent.js"
import { stepKernelV2WithHostEffects } from "./helpers/kernel-v2.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return stepKernelV2WithHostEffects(rt as never, event) as {
    actions: Array<Record<string, unknown>>
    observations: Array<{ kind: string; nodes?: WorkflowSpawnInfo[]; node_outcomes?: Array<{ node_id: string; status: string }> }>
  }
}

function complete(rt: { step(json: string): string }, agentId: string) {
  return step(rt, {
    kind: "sub_agent_completed",
    result: {
      agent_id: agentId,
      result: {
        termination: "completed",
        final_message: { role: "assistant", content: "ok", tool_calls: [] },
        turns_used: 1,
        total_tokens_used: 1,
      },
    },
  })
}

describe("submitWorkflowNodesToKernel", () => {
  it("maps host nodes → the submit_workflow_nodes kernel event (string-task shorthand + defaults)", () => {
    const event = submitWorkflowNodesToKernel([{ task: "more", role: "implement" }])
    expect(event).toEqual({
      kind: "submit_workflow_nodes",
      nodes: [
        {
          task: { goal: "more", criteria: [] },
          role: "implement",
          isolation: "shared",
          context_inheritance: "none",
          dep_policy: "all_success",
        },
      ],
    })
  })

  it("carries trust + batch-relative deps through to the kernel shape", () => {
    const event = submitWorkflowNodesToKernel([
      { task: { goal: "extract", criteria: [] }, role: "explore", trust: "quarantined" },
      { task: "verify", role: "verify", dependsOn: [0] },
    ]) as { nodes: Array<Record<string, unknown>> }
    expect(event.nodes[0]).toMatchObject({ trust: "quarantined", role: "explore" })
    expect(event.nodes[1]).toMatchObject({ depends_on: [0], role: "verify" })
  })

  it("G1: stamps submitter_agent_id only when provided", () => {
    expect(submitWorkflowNodesToKernel([{ task: "x", role: "implement" }])).not.toHaveProperty(
      "submitter_agent_id",
    )
    expect(submitWorkflowNodesToKernel([{ task: "x", role: "implement" }], "wf-node0")).toMatchObject({
      submitter_agent_id: "wf-node0",
    })
  })
})

describe("G1: quarantined submitter cannot escalate over the kernel ABI", () => {
  it("coerces a quarantined submitter's node to quarantined → the spawn-time gate denies write isolation", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    // A workflow whose only node is QUARANTINED (it reads untrusted content), read-only so it spawns.
    step(rt, {
      kind: "load_workflow",
      spec: {
        nodes: [
          {
            task: { goal: "read-untrusted", criteria: [] },
            role: "explore",
            isolation: "read_only",
            context_inheritance: "none",
            trust: "quarantined",
          },
        ],
      },
      parent_session_id: "sess",
    })

    // The quarantined node submits a node it declares write-capable (default `shared`). WITHOUT a
    // submitter id the node would spawn; WITH the quarantined submitter, the kernel coerces it to
    // quarantined and the spawn-time quarantine gate then denies it — so no wf-node1 appears.
    const escalated = step(
      rt,
      submitWorkflowNodesToKernel([{ task: "act-with-privilege", role: "implement" }], "wf-node0"),
    )
    expect(
      escalated.observations.some(
        o => o.kind === "workflow_batch_spawned" && o.nodes?.some(n => n.agent_id === "wf-node1"),
      ),
    ).toBe(false)
  })

  it("control: the identical node from a trusted (absent) submitter spawns normally", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })
    step(rt, {
      kind: "load_workflow",
      spec: {
        nodes: [
          { task: { goal: "root", criteria: [] }, role: "implement", isolation: "shared", context_inheritance: "none" },
        ],
      },
      parent_session_id: "sess",
    })
    // No submitter id → no coercion → the write-capable node spawns as wf-node1.
    const spawned = step(rt, submitWorkflowNodesToKernel([{ task: "act-with-privilege", role: "implement" }]))
    expect(
      spawned.observations.some(
        o => o.kind === "workflow_batch_spawned" && o.nodes?.some(n => n.agent_id === "wf-node1"),
      ),
    ).toBe(true)
  })
})

describe("submit_workflow_nodes over the kernel ABI", () => {
  it("appends a node to a running workflow and keeps it alive until the appended node completes", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    // A single-node workflow: wf-node0 spawns first.
    const spec: WorkflowSpec = { nodes: [{ task: "root", role: "implement" }] }
    const loaded = step(rt, {
      kind: "load_workflow",
      spec: { nodes: [{ task: { goal: "root", criteria: [] }, role: "implement", isolation: "shared", context_inheritance: "none" }] },
      parent_session_id: "sess",
    })
    expect(
      loaded.observations.some(o => o.kind === "workflow_batch_spawned" && o.nodes?.some(n => n.agent_id === "wf-node0")),
    ).toBe(true)
    void spec

    // Submit a node over the ABI while wf-node0 runs → it spawns immediately as wf-node1.
    const submitted = step(rt, submitWorkflowNodesToKernel([{ task: "more", role: "implement" }]))
    expect(
      submitted.observations.some(
        o => o.kind === "workflow_batch_spawned" && o.nodes?.some(n => n.agent_id === "wf-node1" && n.goal === "more"),
      ),
    ).toBe(true)

    // The workflow finishes only after BOTH the root and the submitted node complete.
    const afterRoot = complete(rt, "wf-node0")
    expect(afterRoot.observations.some(o => o.kind === "workflow_completed")).toBe(false)
    const done = complete(rt, "wf-node1")
    const completed = done.observations.find(o => o.kind === "workflow_completed")
    expect(completed?.node_outcomes?.map(node => node.node_id).sort()).toEqual(["wf-node0", "wf-node1"])
  })

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
