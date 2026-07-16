import { getKernel } from "../src/kernel.js"
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { AgentRunSpec, StreamEvent } from "../src/index.js"
import { durableStartKernelV2, durableStepKernelV2 } from "./helpers/kernel-v2.js"

describe("RuntimeRunner.spawnSubAgent governance result", () => {
  it("yields the committed denial instead of throwing a missing-observation error", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({ sessionLog, maxTokens: 8000 } as never)
    const runtime = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    await durableStartKernelV2(runtime, sessionLog, "spawn-denied")
    await durableStepKernelV2(runtime, sessionLog, "spawn-denied", {
      kind: "set_resource_quota",
      quota: { max_spawn_depth: 0 },
    })
    ;(runner as never as { activeKernel: unknown }).activeKernel = runtime
    ;(runner as never as { currentSessionId: string }).currentSessionId = "spawn-denied"
    ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []

    const spec: AgentRunSpec = {
      identity: { agentId: "worker", sessionId: "worker-session", isSubAgent: true },
      role: "implement",
      isolation: "shared",
      goal: "work",
    }
    const events: StreamEvent[] = []
    for await (const event of runner.spawnSubAgent(spec)) events.push(event)

    expect(events).toEqual([
      expect.objectContaining({
        type: "error",
        message: expect.stringContaining("max_spawn_depth"),
      }),
    ])
  })
})
