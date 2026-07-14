import { mkdtemp } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { ProcessSandboxPlane } from "../../src/runtime/process-sandbox-plane.js"
import type { OperationContext } from "../../src/runtime/reliability.js"

it("terminates sandbox subprocess work when the operation is already cancelled", async () => {
  const dir = await mkdtemp(join(tmpdir(), "deepstrike-cancel-"))
  const plane = new ProcessSandboxPlane({ sandboxDir: dir, timeoutMs: 30_000 })
  const controller = new AbortController()
  controller.abort("operation cancelled")
  const operation: OperationContext = {
    runId: "run",
    sessionId: "session",
    signal: controller.signal,
  }
  const events = []

  for await (const event of plane.executeAll([
    { id: "call", name: "run_bash", arguments: JSON.stringify({ command: "sleep 10" }) },
  ], { operation })) events.push(event)

  expect(events).toHaveLength(1)
  expect(events[0]).toMatchObject({ type: "tool_result", isError: false })
  expect((events[0] as { content: string }).content).toContain("operation cancelled")
})
