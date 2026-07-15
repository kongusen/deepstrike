import { getKernel } from "../../src/kernel.js"
import { stepKernelV2 } from "../helpers/kernel-v2.js"

describe("RenderedContext budget evidence", () => {
  it("exposes fixed-context overflow through the native binding", () => {
    const runtime = new (getKernel().KernelRuntime)({ maxTokens: 16 })
    stepKernelV2(runtime, {
      kind: "add_system_message",
      content: "fixed ".repeat(200),
      tokens: 300,
    })

    expect(runtime.render().budgetOverflow).toEqual({
      kind: "fixed_context",
      requiredTokens: 300,
      maxTokens: 16,
    })
  })
})
