import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { AgentRunSpec } from "../src/index.js"

describe("RuntimeRunner.spawnSubAgent canonical cutover", () => {
  it("rejects direct host child authorship", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({ sessionLog, maxTokens: 8000 } as never)

    const spec: AgentRunSpec = {
      identity: { agentId: "worker", sessionId: "worker-session", isSubAgent: true },
      role: "implement",
      isolation: "shared",
      goal: "work",
    }
    await expect((async () => {
      for await (const _event of runner.spawnSubAgent(spec)) {
        // The canonical cutover rejects before any stream event exists.
      }
    })()).rejects.toThrow(/canonical ABI v3.*provider syscall/)
  })
})
