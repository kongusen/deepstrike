/**
 * G2 deterministic compute: a `NodeKind::Reduce` node runs no LLM agent — the kernel hands the SDK a
 * reducer name + its dependency outputs, and the runner runs the registered pure function.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import { builtinReducers } from "../src/workflow/public.js"
import type { WorkflowSpec } from "../src/index.js"
import { workflowNodeSpecToKernel } from "../src/types/agent.js"

describe("built-in reducers", () => {
  it("dedupe_lines unions lines first-seen across inputs", () => {
    const out = builtinReducers.dedupe_lines([
      { agentId: "a", output: "x\ny\nx" },
      { agentId: "b", output: "y\nz" },
    ])
    expect(out).toBe("x\ny\nz")
  })
  it("merge_json_arrays concatenates and dedupes by canonical JSON", () => {
    const out = builtinReducers.merge_json_arrays([
      { agentId: "a", output: '[{"id":1},{"id":2}]' },
      { agentId: "b", output: '[{"id":2},{"id":3}]' },
    ])
    expect(JSON.parse(out)).toEqual([{ id: 1 }, { id: 2 }, { id: 3 }])
  })
  it("concat joins with blank lines; count counts non-empty inputs", () => {
    expect(builtinReducers.concat([{ agentId: "a", output: "A" }, { agentId: "b", output: "B" }])).toBe("A\n\nB")
    expect(builtinReducers.count([{ agentId: "a", output: "x" }, { agentId: "b", output: "  " }])).toBe("1")
  })
})

describe("workflowNodeSpecToKernel lowers a reducer to NodeKind::Reduce", () => {
  it("emits kind {type: reduce, reducer}", () => {
    const k = workflowNodeSpecToKernel({ task: "merge", role: "implement", reducer: "dedupe_lines", dependsOn: [0, 1] })
    expect(k.kind).toEqual({ type: "reduce", reducer: "dedupe_lines" })
    expect(k.depends_on).toEqual([0, 1])
    expect(k.dep_policy).toBe("all_success")
    expect(workflowNodeSpecToKernel({ task: "merge", role: "implement", depPolicy: "accept_partial" }).dep_policy)
      .toBe("accept_partial")
  })
})

describe("canonical runWorkflow rejects unrepresentable reduce nodes", () => {
  it("does not silently downgrade a reducer to an LLM node", async () => {
    let agentCalls = 0
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string } }) {
        agentCalls += 1
        const id = ctx.manifest.agent_id
        // Each worker emits two lines; one overlaps so dedupe must collapse it.
        const content = id === "wf-node0" ? "alpha\nshared" : "shared\nbeta"
        return {
          agentId: id,
          result: { termination: "completed", finalMessage: { role: "assistant", content, toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 },
        }
      },
    }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      sessionLog,
      maxTokens: 8000,
      subAgentOrchestrator: orchestrator as never,
    } as never)

    const spec: WorkflowSpec = {
      nodes: [
        { task: "worker A", role: "explore" },
        { task: "worker B", role: "explore" },
        { task: "merge", role: "implement", reducer: "dedupe_lines", dependsOn: [0, 1] },
      ],
    }

    const outcome = await runner.runWorkflow(spec, { sessionId: "wf-g2" })

    expect(outcome.nodeOutcomes).toEqual([])
    expect(outcome.rejection).toMatchObject({
      operation: "start_workflow",
      reason: expect.stringContaining("absent from canonical WorkflowNode: kind"),
    })
    expect(agentCalls).toBe(0)
  })

  it("rejects an unknown reducer before any child is launched", async () => {
    let agentCalls = 0
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string } }) {
        agentCalls += 1
        return { agentId: ctx.manifest.agent_id, result: { termination: "completed", finalMessage: { role: "assistant", content: "x", toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 } }
      },
    }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({ sessionLog, maxTokens: 8000, subAgentOrchestrator: orchestrator as never } as never)

    const spec: WorkflowSpec = {
      nodes: [
        { task: "worker", role: "explore" },
        { task: "merge", role: "implement", reducer: "does_not_exist", dependsOn: [0] },
      ],
    }
    const outcome = await runner.runWorkflow(spec, { sessionId: "wf-g2b" })
    expect(outcome.nodeOutcomes).toEqual([])
    expect(outcome.rejection?.reason).toContain("absent from canonical WorkflowNode: kind")
    expect(agentCalls).toBe(0)
  })
})
