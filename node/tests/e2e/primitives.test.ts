/**
 * Real-model end-to-end for the workflow DAG: the verify_rules verification shape, driving the
 * kernel state machine against a live LLM. (The former standalone tournament / loop-until-done
 * primitives were removed in A#1 in favor of NodeKind::Tournament / NodeKind::Loop, driven through
 * the same workflow executor; their kernel coverage lives in the Rust suite.)
 *
 * Requires a provider key. Run with:
 *   set -a; source .env; set +a; npx jest e2e/primitives --testTimeout 300000
 * Skips cleanly when no key is present.
 */
import {
  RuntimeRunner,
  InMemorySessionLog,
  LocalExecutionPlane,
} from "../../src/index.js"
import type { WorkflowSpec } from "../../src/index.js"
import { getKernel } from "../../src/kernel.js"
import { loadProviders, anyProvider } from "./providers.js"
import { startKernelV2 } from "../helpers/kernel-v2.js"

const provider = anyProvider(loadProviders())
const maybe = provider ? describe : describe.skip

maybe("real-model workflow DAG", () => {
  it("verify_rules: live verifiers + skeptic run as a gated workflow DAG", async () => {
    const runner = new RuntimeRunner({
      provider: provider!,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      maxTurns: 4,
    })
    const kernel = new (getKernel().KernelRuntime)({ maxTokens: 8000 })
    startKernelV2(kernel, "review")
    ;(runner as any).activeKernel = kernel
    ;(runner as any).currentSessionId = "verify-e2e"
    ;(runner as any).pendingObservations = []

    // The shape verify_rules(rules, skeptic) produces: one verify node per rule + a skeptic
    // depending on all of them. Verifiers run with no inherited author context (bias-resistant).
    const spec: WorkflowSpec = {
      nodes: [
        { task: "Check this rule against `price = 9.99` (float money): is it violated? Answer briefly.", role: "verify" },
        { task: "Check this rule against `catch(e){}` (errors must propagate): is it violated? Answer briefly.", role: "verify" },
        { task: "Skeptic: given the two verifier findings, list only the real violations.", role: "verify", dependsOn: [0, 1] },
      ],
    }

    const outcome = await runner.runWorkflow(spec)
    expect(outcome.completed.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
    expect(outcome.failed).toEqual([])
  }, 300_000)
})
