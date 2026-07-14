import { ManagedTaskScope } from "../../src/runtime/reliability.js"
import type { OperationContext } from "../../src/runtime/reliability.js"

describe("ManagedTaskScope", () => {
  const operation = (): OperationContext => ({
    runId: "run-1",
    sessionId: "session-1",
    agentId: "agent-1",
    signal: new AbortController().signal,
  })

  it("drains owned work before closing", async () => {
    const completed: string[] = []
    const scope = new ManagedTaskScope(operation())
    scope.spawn("persist-summary", async () => {
      await Promise.resolve()
      completed.push("persisted")
    })

    await scope.drain()

    expect(completed).toEqual(["persisted"])
    expect(scope.pending).toBe(0)
  })

  it("reports task failures with immutable operation identity", async () => {
    const failures: Array<{ label: string; runId: string }> = []
    const scope = new ManagedTaskScope(operation(), failure => {
      failures.push({ label: failure.label, runId: failure.operation.runId })
    })
    scope.spawn("semantic-page-out", async () => { throw new Error("store unavailable") })

    await scope.drain()

    expect(failures).toEqual([{ label: "semantic-page-out", runId: "run-1" }])
  })

  it("rejects new work after the scope is closed", async () => {
    const scope = new ManagedTaskScope(operation())
    await scope.drain()

    expect(() => scope.spawn("late", async () => {})).toThrow("task scope is closed")
  })
})
