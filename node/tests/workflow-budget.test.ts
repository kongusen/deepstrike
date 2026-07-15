/**
 * G4 budget-as-signal: the kernel reports remaining workflow headroom on `workflow_batch_spawned`,
 * and the runner surfaces it into a coordinator node's goal so it can size its submission.
 */
import { getKernel } from "../src/kernel.js"
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { WorkflowSpec } from "../src/index.js"
import { workflowBudgetNote, type WorkflowBudget } from "../src/types/agent.js"
import { startKernelV2, stepKernelV2 } from "./helpers/kernel-v2.js"

describe("workflowBudgetNote", () => {
  it("formats bounded dimensions and omits unbounded ones", () => {
    const full: WorkflowBudget = {
      nodes_used: 1,
      nodes_max: 5,
      nodes_remaining: 4,
      running_subagents: 1,
      max_concurrent_subagents: 3,
      concurrency_remaining: 2,
      tokens_used: 2500,
      tokens_max: 10000,
      tokens_remaining: 7500,
    }
    const note = workflowBudgetNote(full)
    expect(note).toContain("nodes 1/5 used, 4 remaining")
    expect(note).toContain("concurrency 1/3 running, 2 free")
    // M4/G5: token headroom is surfaced so a coordinator can scale to "use N tokens".
    expect(note).toContain("tokens 2500/10000 used, 7500 remaining")

    // No quota ⇒ no signal.
    expect(workflowBudgetNote(undefined)).toBe("")
    expect(workflowBudgetNote({ nodes_used: 2, running_subagents: 1 })).toBe("")
  })
})

describe("runWorkflow surfaces the kernel budget into a node's goal", () => {
  it("appends the remaining-budget note when a resource quota is installed", async () => {
    const goals: string[] = []
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string }; spec: { goal: string } }) {
        goals.push(ctx.spec.goal)
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

    // A real kernel with a node/concurrency quota installed (so a budget is emitted).
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    startKernelV2(rt)
    stepKernelV2(rt, { kind: "set_resource_quota", quota: { max_workflow_nodes: 5, max_concurrent_subagents: 3 } })
    ;(runner as never as { activeKernel: unknown }).activeKernel = rt
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-g4"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    const spec: WorkflowSpec = { nodes: [{ task: "coordinate", role: "implement" }] }
    await runner.runWorkflow(spec)

    expect(goals).toHaveLength(1)
    expect(goals[0]).toContain("[workflow budget]")
    expect(goals[0]).toContain("nodes 1/5 used, 4 remaining")
    // M4/G5: the kernel now also reports token headroom (cap always set on the scheduler budget).
    expect(goals[0]).toMatch(/tokens \d+\/\d+ used, \d+ remaining/)
  })
})
