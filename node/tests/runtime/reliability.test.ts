import { ManagedTaskScope, operationAbortSignal } from "../../src/runtime/reliability.js"
import type { OperationContext } from "../../src/runtime/reliability.js"

const operation = (): OperationContext => ({
  runId: "run-1",
  sessionId: "session-1",
  agentId: "agent-1",
  signal: new AbortController().signal,
})

describe("ManagedTaskScope", () => {
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

describe("operationAbortSignal", () => {
  it("propagates operation cancellation into adapter work", () => {
    const controller = new AbortController()
    const signal = operationAbortSignal({ ...operation(), signal: controller.signal }, 30_000)

    controller.abort("stop")

    expect(signal.aborted).toBe(true)
  })

  it("uses an already-expired operation deadline", () => {
    const signal = operationAbortSignal({ ...operation(), deadlineMs: Date.now() - 1 }, 30_000)
    expect(signal.aborted).toBe(true)
  })
})
