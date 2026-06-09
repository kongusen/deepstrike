/**
 * Real-model end-to-end: drive a workflow DAG through the kernel with a live LLM.
 *
 * The kernel owns the DAG and gates each node spawn; the default sub-agent orchestrator runs
 * each node as a real child RuntimeRunner against the configured provider, feeding results back
 * until the kernel reports the workflow complete.
 *
 * Requires a provider key in the environment. Run with:
 *   set -a; source .env; set +a; npx jest e2e/workflow --testTimeout 300000
 * Skips cleanly when no key is present.
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../../src/index.js"
import type { WorkflowSpec } from "../../src/index.js"
import { getKernel } from "../../src/kernel.js"
import { loadProviders, anyProvider } from "./providers.js"

const provider = anyProvider(loadProviders())
const maybe = provider ? describe : describe.skip

maybe("real-model workflow drive", () => {
  it("runs a fanout→synthesize DAG against a live model", async () => {
    const runner = new RuntimeRunner({
      provider: provider!,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      maxTurns: 4,
    })

    // Establish an active parent run on a kernel (runWorkflow runs mid-run).
    const kernel = new (getKernel().KernelRuntime)({ maxTokens: 8000 })
    kernel.step(JSON.stringify({ version: 1, event: { kind: "start_run", task: { goal: "parent", criteria: [] } } }))
    // The runner drives the workflow on this kernel; node specs run via the orchestrator.
    ;(runner as any).activeKernel = kernel
    ;(runner as any).currentSessionId = "wf-e2e"
    ;(runner as any).pendingObservations = []

    const spec: WorkflowSpec = {
      nodes: [
        { task: "Reply with exactly one word: APPLE", role: "explore" },
        { task: "Reply with exactly one word: BANANA", role: "explore" },
        { task: "In one short sentence, name a recipe using the two fruits the workers found.", role: "plan", dependsOn: [0, 1] },
      ],
    }

    const outcome = await runner.runWorkflow(spec)
    expect(outcome.completed.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
    expect(outcome.failed).toEqual([])
  }, 300_000)
})
