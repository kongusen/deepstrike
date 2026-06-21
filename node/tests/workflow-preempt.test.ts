/**
 * #2-B-ii (end-to-end): a Critical `InterruptNow` signal arriving WHILE a workflow node is running
 * preempts it mid-flight. The drive loop's concurrent monitor polls the signal source during the
 * batch → routes it to the kernel (root suspended in `SubAgentAwait` → preempt) → the kernel emits
 * `AgentPreempted` + tears the workflow down → the matching child's `AbortSignal` fires, cancelling
 * its in-flight LLM call. Real native kernel; mock orchestrator whose child blocks until aborted.
 */
import { getKernel } from "../src/kernel.js"
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/index.js"
import { SignalGateway } from "../src/os/public.js"
import type { WorkflowSpec } from "../src/index.js"

describe("#2-B-ii mid-flight workflow preemption", () => {
  it("a Critical signal aborts the running node and tears the workflow down", async () => {
    const orch = {
      sawAbort: false,
      // The node "runs" until its parent-controlled abort signal fires (or a safety timeout).
      async run(ctx: { spec: { identity: { agentId: string } }; abortSignal?: AbortSignal }) {
        await new Promise<void>(resolve => {
          const s = ctx.abortSignal
          if (s?.aborted) return resolve()
          const t = setTimeout(resolve, 2000) // safety net so the test can't hang
          s?.addEventListener("abort", () => { clearTimeout(t); resolve() }, { once: true })
        })
        orch.sawAbort = ctx.abortSignal?.aborted ?? false
        return {
          agentId: ctx.spec.identity.agentId,
          result: { termination: "user_abort", finalMessage: { role: "assistant", content: "aborted", toolCalls: [] }, turnsUsed: 0, totalTokensUsed: 0 },
        }
      },
    }

    const gateway = new SignalGateway()
    // Queue a Critical signal so the batch monitor picks it up on its first poll, mid-run.
    gateway.ingest({ source: "gateway", signalType: "alert", urgency: "critical", payload: { goal: "STOP NOW" } })

    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      subAgentOrchestrator: orch as never,
      signalSource: gateway,
    } as never)

    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    rt.step(JSON.stringify({ version: 1, event: { kind: "start_run", task: { goal: "parent", criteria: [] } } }))
    ;(runner as never as { activeKernel: unknown }).activeKernel = rt
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-preempt"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    const spec: WorkflowSpec = { nodes: [{ task: "a long-running node", role: "implement" }] }
    const outcome = await runner.runWorkflow(spec)

    // The running node was aborted mid-flight and the workflow torn down.
    expect(orch.sawAbort).toBe(true)
    expect(outcome.failed).toContain("wf-node0")
    const pending = (runner as never as { pendingObservations: Array<{ kind: string; agent_ids?: string[] }> }).pendingObservations
    const preempt = pending.find(o => o.kind === "agent_preempted")
    expect(preempt).toBeDefined()
    expect(preempt?.agent_ids).toContain("wf-node0")
  })
})
