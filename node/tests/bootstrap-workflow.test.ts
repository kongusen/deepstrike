/**
 * M5 v2 / G1: an agent authors a top-level workflow. `bootstrapWorkflow` routes a host `WorkflowSpec`
 * through the agent-reachable `Syscall::LoadWorkflow` (the `submit_workflow` kernel event): with no
 * workflow active the kernel BOOTSTRAPS the DAG in this same kernel (unified governance — one kernel,
 * one quota), then the shared driver runs it to completion. Exercises the real native ABI end-to-end.
 */
import { getKernel } from "../src/kernel.js"
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import { submitWorkflowToKernel } from "../src/types/agent.js"
import type { WorkflowSpec } from "../src/index.js"
import { startKernelV2, stepKernelV2 } from "./helpers/kernel-v2.js"

describe("submitWorkflowToKernel", () => {
  it("lowers a spec to the submit_workflow event with the parent session id", () => {
    const ev = submitWorkflowToKernel({ nodes: [{ task: "x", role: "implement" }] }, "sess-1")
    expect(ev.kind).toBe("submit_workflow")
    expect(ev.parent_session_id).toBe("sess-1")
    expect((ev.spec as { nodes: unknown[] }).nodes).toHaveLength(1)
    // submitter id only when a quarantined author needs trust coercion (flatten case).
    expect(ev.submitter_agent_id).toBeUndefined()
    expect(submitWorkflowToKernel({ nodes: [] }, "s", "wf-node3").submitter_agent_id).toBe("wf-node3")
  })
})

describe("bootstrapWorkflow drives an agent-authored DAG over the real kernel", () => {
  it("bootstraps a workflow when none is active and runs every authored node to completion", async () => {
    const ran: string[] = []
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string }; spec: { goal: string } }) {
        ran.push(ctx.manifest.agent_id)
        return {
          agentId: ctx.manifest.agent_id,
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
      subAgentOrchestrator: orchestrator as never,
    } as never)

    // A real kernel with NO workflow loaded — the agent itself authors one via submit_workflow.
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    startKernelV2(rt)
    ;(runner as never as { activeKernel: unknown }).activeKernel = rt
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-boot"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    // Two independent nodes + one reduce dependent — a real little DAG the agent designed.
    const spec: WorkflowSpec = {
      nodes: [
        { task: "explore A", role: "implement" },
        { task: "explore B", role: "implement" },
      ],
    }

    const outcome = await runner.bootstrapWorkflow(spec)

    // The authored nodes bootstrapped + ran in this same kernel (no separate child kernel).
    expect(ran.sort()).toEqual(["wf-node0", "wf-node1"])
    expect(outcome.completed.sort()).toEqual(["wf-node0", "wf-node1"])
    expect(outcome.failed).toEqual([])
  })

  it("is denied when the authored spec would overgrow the workflow-node quota", async () => {
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

    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    startKernelV2(rt)
    stepKernelV2(rt, { kind: "set_resource_quota", quota: { max_workflow_nodes: 2 } })
    ;(runner as never as { activeKernel: unknown }).activeKernel = rt
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-boot-deny"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    // 3 nodes > max(2) → the kernel denies the bootstrap; nothing runs.
    const spec: WorkflowSpec = {
      nodes: [
        { task: "a", role: "implement" },
        { task: "b", role: "implement" },
        { task: "c", role: "implement" },
      ],
    }
    const outcome = await runner.bootstrapWorkflow(spec)
    expect(ran).toEqual([])
    expect(outcome.completed).toEqual([])
  })
})
