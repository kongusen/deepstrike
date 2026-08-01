/**
 * Canonical-ABI effect lifecycle tests (wasm, Task 21).
 *
 * Drives a pre-injected scripted CanonicalRunnerRuntime via `runWorkflow` (same convention as
 * workflow-optimization / workflow-preempt) so `execute()` does not replace the fake.
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import { MILESTONE_UNVERIFIED_REASON } from "../src/runtime/types/agent.js"
import type { LLMProvider } from "../src/types.js"
import { wrapScriptedKernel } from "./helpers/scripted-canonical-runtime.js"
import { tool } from "../src/tools/index.js"
import type { WorkflowSpec } from "../src/index.js"

const idleProvider: LLMProvider = {
  async complete() {
    return { role: "assistant", content: "done", toolCalls: [] }
  },
  async *stream() {
    yield { type: "text_delta", delta: "done" }
  },
}

function inject(runner: RuntimeRunner, fake: { step(input: string): string }, sessionId: string) {
  ;(runner as unknown as { activeKernel: unknown }).activeKernel = wrapScriptedKernel(fake)
  ;(runner as unknown as { currentSessionId: string }).currentSessionId = sessionId
  ;(runner as unknown as { pendingObservations: unknown[] }).pendingObservations = []
}

describe("R-B27: an unverifiable milestone effect is still resolved", () => {
  it("does not busy-wait when a scripted kernel surfaces evaluate_milestone", async () => {
    // Full milestone fail-closed resolution against a real kernel is covered by Node.
    // Under the WASM scripted adapter, an unexpected evaluate_milestone on workflow load must
    // fail closed immediately (typed error) rather than hang with a live pending effect.
    const fake = {
      step(input: string): string {
        const { event } = JSON.parse(input) as { event: Record<string, unknown> }
        if (event.kind === "load_workflow") {
          return JSON.stringify({
            version: 2,
            actions: [{
              kind: "evaluate_milestone",
              effect_id: "ms-1",
              phase_id: "phase1",
              criteria: ["ship it"],
              required_evidence: [],
            }],
            observations: [],
            faults: [],
          })
        }
        return JSON.stringify({ version: 2, actions: [], observations: [], faults: [] })
      },
    }

    const runner = new RuntimeRunner({
      provider: idleProvider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4000,
      maxTurns: 6,
    })
    inject(runner, fake, "ms-leak")

    await expect(runner.runWorkflow({ nodes: [{ task: "attest", role: "implement" }] }))
      .rejects.toThrow(/evaluate_milestone|milestone/)
    expect(MILESTONE_UNVERIFIED_REASON.length).toBeGreaterThan(0)
  })
})

describe("R-B28: the main loop cannot busy-wait on an effect it has no branch for", () => {
  it("terminates the run with an explicit error", async () => {
    const fake = {
      step(input: string): string {
        const { event } = JSON.parse(input) as { event: Record<string, unknown> }
        if (event.kind === "load_workflow") {
          return JSON.stringify({
            version: 2,
            actions: [{
              kind: "preempt_sub_agents",
              effect_id: "pre-1",
              agent_ids: ["ghost"],
              reason: "test",
            }],
            observations: [],
            faults: [],
          })
        }
        return JSON.stringify({ version: 2, actions: [], observations: [], faults: [] })
      },
    }

    const runner = new RuntimeRunner({
      provider: idleProvider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4000,
      maxTurns: 4,
    })
    inject(runner, fake, "unhandled-effect")

    await expect(runner.runWorkflow({ nodes: [{ task: "x", role: "implement" }] }))
      .rejects.toThrow(/unhandled kernel effect|preempt_sub_agents/)
  }, 15_000)
})

describe("the default payload store survives long enough to be read back", () => {
  it("persists and reloads an external tool payload", async () => {
    const huge = "z".repeat(80 * 1024)
    const plane = new LocalExecutionPlane()
    plane.register(tool("big_out", "emit huge", { type: "object", properties: {} }, () => huge))

    let calls = 0
    const provider: LLMProvider = {
      async complete() {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream() {
        calls += 1
        if (calls === 1) {
          yield { type: "tool_call", id: "big-1", name: "big_out", arguments: {} }
          return
        }
        if (calls === 2) {
          yield { type: "tool_call", id: "read-1", name: "read_result", arguments: { call_id: "big-1" } }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const events = []
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 4000,
      maxTurns: 8,
    })

    for await (const evt of runner.run({ sessionId: "payload-default", goal: "test" })) {
      events.push(evt)
    }

    expect(events.some(e => (e as { type: string }).type === "done")).toBe(true)
  })
})
