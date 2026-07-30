/**
 * G4 budget-as-signal: the kernel reports remaining workflow headroom on `workflow_batch_spawned`,
 * and the runner surfaces it into a coordinator node's goal so it can size its submission.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { WorkflowSpec } from "../src/index.js"
import { workflowBudgetNote, type WorkflowBudget } from "../src/types/agent.js"

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
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      sessionLog,
      maxTokens: 8000,
      resourceQuota: { maxWorkflowNodes: 5, maxConcurrentSubagents: 3 },
      subAgentOrchestrator: orchestrator as never,
    } as never)

    const spec: WorkflowSpec = { nodes: [{ task: "coordinate", role: "implement" }] }
    await runner.runWorkflow(spec, { sessionId: "wf-g4" })

    expect(goals).toHaveLength(1)
    expect(goals[0]).toContain("[workflow budget]")
    expect(goals[0]).toContain("concurrency capped at 3")
    // Canonical v3 publishes the kernel-owned cap, not a host-authored remaining counter.
    expect(goals[0]).toMatch(/tokens capped at \d+/)
  })
})
