/**
 * #2-B-ii (wasm, scripted kernel): a Critical `InterruptNow` during a running workflow batch aborts
 * the running node and tears the workflow down. WASM has no native kernel in tests, so a scripted
 * fake kernel reproduces the `agent_preempted` + `workflow_completed` the Rust kernel emits on preempt;
 * this exercises the SDK side — the concurrent monitor, per-node abort controller, and child abort.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import type { WorkflowSpec } from "../src/index.js"

type Obs = { kind: string; nodes?: unknown[]; completed?: string[]; failed?: string[]; agent_ids?: string[]; reason?: string }
const node = (agent_id: string, goal: string) => ({
  agent_id, goal, role: "implement", isolation: "shared", context_inheritance: "none", model_hint: null, trust: "trusted",
})

/** Scripted kernel: load spawns wf-node0; a `signal` (the monitor's Critical) preempts it. */
function makeFakeKernel() {
  const reply = (obs: Obs[]) => JSON.stringify({ version: 1, actions: [], observations: obs })
  return {
    turn: () => 0,
    step(input: string): string {
      const { event } = JSON.parse(input) as { event: { kind: string } }
      if (event.kind === "load_workflow") return reply([{ kind: "workflow_batch_spawned", nodes: [node("wf-node0", "A")] }])
      if (event.kind === "signal") {
        return reply([
          { kind: "agent_preempted", agent_ids: ["wf-node0"], reason: "STOP" },
          { kind: "workflow_completed", completed: [], failed: ["wf-node0"] },
        ])
      }
      return reply([])
    },
  }
}

describe("#2-B-ii wasm workflow preemption (scripted kernel)", () => {
  it("a Critical signal aborts the running node mid-batch and tears the workflow down", async () => {
    const orch = {
      sawAbort: false,
      async run(ctx: { spec: { identity: { agentId: string } }; abortSignal?: AbortSignal }) {
        await new Promise<void>(resolve => {
          const s = ctx.abortSignal
          if (s?.aborted) return resolve()
          const t = setTimeout(resolve, 2000)
          s?.addEventListener("abort", () => { clearTimeout(t); resolve() }, { once: true })
        })
        orch.sawAbort = ctx.abortSignal?.aborted ?? false
        return { agentId: ctx.spec.identity.agentId, result: { termination: "user_abort", finalMessage: { role: "assistant", content: "aborted", toolCalls: [] }, turnsUsed: 0, totalTokensUsed: 0 } }
      },
    }
    const signalSource = {
      pending: [{ source: "gateway", signalType: "alert", urgency: "critical", payload: { goal: "STOP NOW" } }] as unknown[],
      async nextSignal() { return (this.pending.shift() as never) ?? null },
    }

    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      subAgentOrchestrator: orch as never,
      signalSource: signalSource as never,
    } as never)
    ;(runner as never as { activeKernel: unknown }).activeKernel = makeFakeKernel()
    ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-preempt"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    const spec: WorkflowSpec = { nodes: [{ task: "a long-running node", role: "implement" }] }
    const outcome = await runner.runWorkflow(spec)

    expect(orch.sawAbort).toBe(true)
    expect(outcome.failed).toContain("wf-node0")
  })
})
