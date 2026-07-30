/**
 * M5 v2 / G1: an agent authors a top-level workflow. `bootstrapWorkflow` routes a host `WorkflowSpec`
 * through the agent-reachable `Syscall::LoadWorkflow` (the `submit_workflow` kernel event): with no
 * workflow active the kernel BOOTSTRAPS the DAG in this same kernel (unified governance — one kernel,
 * one quota), then the shared driver runs it to completion. Exercises the real native ABI end-to-end.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import { submitWorkflowToKernel } from "../src/types/agent.js"
import type { WorkflowSpec } from "../src/index.js"

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

describe("bootstrapWorkflow canonical cutover", () => {
  it("rejects direct host authorship and points callers to the provider syscall", async () => {
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
    } as never)
    const spec: WorkflowSpec = {
      nodes: [{ task: "explore A", role: "implement" }],
    }

    await expect(runner.bootstrapWorkflow(spec)).rejects.toThrow(
      /canonical ABI v3.*provider syscall/,
    )
  })
})
