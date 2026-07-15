/**
 * Standalone `runWorkflow` — the stateless-handler path. With no active `run()`, `runWorkflow` must
 * auto-bootstrap a kernel that owns the DAG (start_run + governance/quota policies), drive it, then
 * tear the kernel down so the runner is reusable. Previously this threw "requires an active parent
 * run" and callers had to poke `(runner as any).activeKernel` by hand.
 *
 * Uses a stub orchestrator so no LLM is needed — the focus is the bootstrap / teardown / resume wiring.
 */
import { RuntimeRunner, InMemorySessionLog, InMemoryGroupBudgetStore } from "../src/index.js"
import type { SessionEvent, WorkflowSpec } from "../src/index.js"

function stubOrchestrator(onCall?: () => void) {
  return {
    async run(ctx: { manifest: { agent_id: string } }) {
      onCall?.()
      const id = ctx.manifest.agent_id
      return {
        agentId: id,
        result: { termination: "completed", finalMessage: { role: "assistant", content: id, toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 },
      }
    },
  }
}

const fanoutSpec: WorkflowSpec = {
  nodes: [
    { task: "worker A", role: "explore" },
    { task: "worker B", role: "explore" },
    { task: "synthesize", role: "plan", dependsOn: [0, 1] },
  ],
}

describe("runWorkflow bootstraps standalone (no active parent run)", () => {
  it("does not start nodes when the durable run_started fact cannot be recorded", async () => {
    let calls = 0
    class FailingStartLog extends InMemorySessionLog {
      override async append(sessionId: string, event: SessionEvent): Promise<number> {
        if (event.kind === "run_started") throw new Error("session log unavailable")
        return super.append(sessionId, event)
      }
    }
    const runner = new RuntimeRunner({
      sessionLog: new FailingStartLog(),
      maxTokens: 8000,
      subAgentOrchestrator: stubOrchestrator(() => { calls++ }) as never,
    } as never)

    await expect(runner.runWorkflow(fanoutSpec, { sessionId: "durable-start" }))
      .rejects.toThrow("session log unavailable")
    expect(calls).toBe(0)
    expect((runner as never as { activeKernel: unknown }).activeKernel).toBeNull()
    expect((runner as never as { currentSessionId: unknown }).currentSessionId).toBeNull()
  })

  it("drives a fanout→synthesize DAG with a bare runner — no activeKernel hack", async () => {
    let calls = 0
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: stubOrchestrator(() => { calls++ }) as never,
    } as never)

    const outcome = await runner.runWorkflow(fanoutSpec)

    expect(outcome.completed.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
    expect(outcome.failed).toEqual([])
    expect(calls).toBe(3)
    // Every node's output is surfaced back to the host.
    expect(outcome.outputs["wf-node2"]).toBe("wf-node2")
  })

  it("settles standalone workflow node usage from the kernel terminal report", async () => {
    const store = new InMemoryGroupBudgetStore()
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      resourceQuota: { maxTotalSubagents: 4 },
      runGroup: { id: "workflow-budget", budgetStore: store },
      subAgentOrchestrator: stubOrchestrator() as never,
    } as never)

    await runner.runWorkflow(fanoutSpec, { sessionId: "workflow-member" })

    expect(store.read("workflow-budget").subagentsSpawned).toBe(3)
    expect((await store.members("workflow-budget")).map(member => member.sessionId)).toEqual([
      "workflow-member",
    ])
  })

  it("applies bounded SDK reliability policy during bootstrap", async () => {
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      kernelReliability: {
        eventReplayCapacity: 512,
        completedEffectReplayCapacity: 256,
        hostEffectRetryAttempts: 4,
        spoolThresholdBytes: 2048,
        spoolPreviewBytes: 256,
      },
      subAgentOrchestrator: stubOrchestrator() as never,
    } as never)

    await expect(runner.runWorkflow(fanoutSpec)).resolves.toMatchObject({ failed: [] })
  })

  it("rejects out-of-bounds SDK reliability policy atomically", async () => {
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      kernelReliability: { eventReplayCapacity: 0 },
      subAgentOrchestrator: stubOrchestrator() as never,
    } as never)

    await expect(runner.runWorkflow(fanoutSpec)).rejects.toThrow(/invalid_config/)
  })

  it("tears the bootstrapped kernel down so the runner is reusable across sequential runs", async () => {
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: stubOrchestrator() as never,
    } as never)

    await runner.runWorkflow(fanoutSpec)
    // No active run leaked after a standalone drive.
    expect((runner as never as { activeKernel: unknown }).activeKernel).toBeNull()
    expect((runner as never as { currentSessionId: unknown }).currentSessionId).toBeNull()

    // A second standalone call on the SAME runner must succeed (re-bootstraps cleanly).
    const second = await runner.runWorkflow(fanoutSpec)
    expect(second.completed.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
  })

  it("resumes a standalone workflow from the session log by sessionId — completed nodes are not re-run", async () => {
    let calls = 0
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      sessionLog,
      maxTokens: 8000,
      subAgentOrchestrator: stubOrchestrator(() => { calls++ }) as never,
    } as never)

    await runner.runWorkflow(fanoutSpec, { sessionId: "resume-me" })
    expect(calls).toBe(3)

    // Resume the same session: the kernel skips already-completed nodes, so no new agent calls.
    const resumed = await runner.resumeWorkflow(fanoutSpec, { sessionId: "resume-me" })
    expect(resumed.completed.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
    expect(calls).toBe(3)
  })

  it("still throws on resume with neither an active run nor a sessionId", async () => {
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: stubOrchestrator() as never,
    } as never)
    await expect(runner.resumeWorkflow(fanoutSpec)).rejects.toThrow(/active parent run or an explicit sessionId/)
  })
})
