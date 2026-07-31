import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { AgentRunSpec } from "../src/index.js"

describe("RuntimeRunner.spawnSubAgent (wasm)", () => {
  it("fails closed outside the canonical provider syscall channel", async () => {
    const runner = new RuntimeRunner({ sessionLog: new InMemorySessionLog(), maxTokens: 8000 } as never)
    ;(runner as never as { activeKernel: unknown }).activeKernel = {
      turn: () => 0,
      preservedRefs: () => [],
      step: () => JSON.stringify({
        version: 2,
        actions: [],
        observations: [{
          kind: "control_request_rejected",
          operation: "spawn_sub_agent",
          subject: "worker",
          reason: "max_spawn_depth=0 exceeded (depth 1)",
        }],
        faults: [],
      }),
    }
    ;(runner as never as { currentSessionId: string }).currentSessionId = "spawn-rejected"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []
    const spec: AgentRunSpec = {
      identity: { agentId: "worker", sessionId: "worker-session", isSubAgent: true },
      role: "implement",
      isolation: "shared",
      goal: "work",
    }

    await expect(runner.spawnSubAgent(spec)).rejects.toThrow(
      "spawnSubAgent is unavailable under canonical ABI v3",
    )
  })
})
