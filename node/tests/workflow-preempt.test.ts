/**
 * #2-B-ii (end-to-end): a Critical `InterruptNow` signal arriving WHILE a workflow node is running
 * preempts it mid-flight. The drive loop's concurrent monitor polls the signal source during the
 * batch → routes it to the kernel (root suspended in `SubAgentAwait` → preempt) → the kernel emits
 * `AgentPreempted` + tears the workflow down → the matching child's `AbortSignal` fires, cancelling
 * its in-flight LLM call. Real native kernel; mock orchestrator whose child blocks until aborted.
 */
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

    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      subAgentOrchestrator: orch as never,
      signalSource: gateway,
    } as never)

    const spec: WorkflowSpec = { nodes: [{ task: "a long-running node", role: "implement" }] }
    const outcome = await runner.runWorkflow(spec, { sessionId: "wf-preempt" })

    // The running node was aborted mid-flight and the workflow torn down.
    expect(orch.sawAbort).toBe(true)
    expect(outcome.nodeOutcomes).toContainEqual(expect.objectContaining({ nodeId: "wf-node0", status: "failed" }))
    const events = await sessionLog.read("wf-preempt")
    const preempt = events.find(({ event }) =>
      event.kind === "kernel_observation" && event.observation_kind === "agent_preempted")
    expect(preempt).toBeDefined()
    expect(preempt?.event).toEqual(expect.objectContaining({
      raw: expect.objectContaining({ agent_ids: expect.arrayContaining(["wf-node0"]) }),
    }))
  })

  it("interrupt() aborts a standalone workflow node through the canonical cancellation arc", async () => {
    let childStarted!: () => void
    const started = new Promise<void>(resolve => { childStarted = resolve })
    const orch = {
      sawAbort: false,
      async run(ctx: { spec: { identity: { agentId: string } }; abortSignal?: AbortSignal }) {
        childStarted()
        await new Promise<void>(resolve => {
          const signal = ctx.abortSignal
          if (signal?.aborted) return resolve()
          const timeout = setTimeout(resolve, 2000)
          signal?.addEventListener("abort", () => {
            clearTimeout(timeout)
            resolve()
          }, { once: true })
        })
        orch.sawAbort = ctx.abortSignal?.aborted ?? false
        return {
          agentId: ctx.spec.identity.agentId,
          result: {
            termination: "user_abort",
            finalMessage: { role: "assistant", content: "aborted", toolCalls: [] },
            turnsUsed: 0,
            totalTokensUsed: 0,
          },
        }
      },
    }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      subAgentOrchestrator: orch as never,
    } as never)

    const running = runner.runWorkflow(
      { nodes: [{ task: "wait until the host stops this operation", role: "implement" }] },
      { sessionId: "wf-host-interrupt" },
    )
    await started
    runner.interrupt("host_shutdown")
    const outcome = await running

    expect(orch.sawAbort).toBe(true)
    expect(outcome.nodeOutcomes).toContainEqual(expect.objectContaining({
      nodeId: "wf-node0",
      status: "failed",
    }))
    const cancellation = (await sessionLog.read("wf-host-interrupt"))
      .map(entry => entry.event)
      .find(event => event.kind === "operation_cancelled")
    expect(cancellation).toMatchObject({ kind: "operation_cancelled", reason: "host_shutdown" })
  })
})
