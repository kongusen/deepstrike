import { RuntimeRunner } from "../src/runtime/runner.js"

describe("RuntimeRunner bounded host policy", () => {
  it("accepts a configured workflow schema validation bound", () => {
    expect(() => new RuntimeRunner({ workflowSchemaValidationAttempts: 3 } as never)).not.toThrow()
  })

  it("rejects an unsafe workflow schema validation bound", () => {
    expect(() => new RuntimeRunner({ workflowSchemaValidationAttempts: 0 } as never)).toThrow(
      /between 1 and 16/,
    )
  })
})
