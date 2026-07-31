import { InMemorySessionLog, RuntimeRunner, submitWorkflowToKernel } from "../src/index.js"

describe("canonical workflow entrypoints (wasm)", () => {
  it("lowers provider-authored workflow specs with their parent session", () => {
    const event = submitWorkflowToKernel({ nodes: [{ task: "x", role: "implement" }] }, "session-1")
    expect(event.kind).toBe("submit_workflow")
    expect(event.parent_session_id).toBe("session-1")
    expect((event.spec as { nodes: unknown[] }).nodes).toHaveLength(1)
  })

  it("fails closed for the retired host-authoring bootstrap API", async () => {
    const runner = new RuntimeRunner({ sessionLog: new InMemorySessionLog(), maxTokens: 8_000 } as never)
    await expect(runner.bootstrapWorkflow({ nodes: [{ task: "x", role: "implement" }] }))
      .rejects.toThrow("bootstrapWorkflow is unavailable under canonical ABI v3")
  })
})
