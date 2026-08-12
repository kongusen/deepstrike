import { Agent } from "../src/agent.js"
import { lowerAgent } from "../src/agent-ir.js"
import { tool } from "../src/tools/index.js"

describe("spc_001-03: AgentSpec IR + lowerAgent()", () => {
  it("maps name/instructions/model straight through", () => {
    const agent = new Agent({ name: "researcher", instructions: "be thorough", model: "claude-opus-5" })
    const spec = lowerAgent(agent)
    expect(spec.name).toBe("researcher")
    expect(spec.instructions).toBe("be thorough")
    expect(spec.model).toBe("claude-opus-5")
  })

  it("folds tools into capabilities, one entry per tool", () => {
    const t1 = tool("t1", "tool one", { type: "object", properties: {} }, () => "ok")
    const t2 = tool("t2", "tool two", { type: "object", properties: {} }, () => "ok")
    const agent = new Agent({ name: "worker", tools: [t1, t2] })
    const spec = lowerAgent(agent)
    expect(spec.capabilities.length).toBe(2)
  })

  it("produces an empty capabilities array when no tools are given", () => {
    const agent = new Agent({ name: "idle" })
    const spec = lowerAgent(agent)
    expect(spec.capabilities).toEqual([])
  })

  it("carries providerOptions through unchanged", () => {
    const agent = new Agent({ name: "vendorish", providerOptions: { openai: { reasoningEffort: "high" } } })
    const spec = lowerAgent(agent)
    expect(spec.providerOptions).toEqual({ openai: { reasoningEffort: "high" } })
  })

  it("is a pure function: calling it twice on the same Agent yields deep-equal, independently-mutable results", () => {
    const agent = new Agent({ name: "researcher", tools: [tool("t", "d", { type: "object", properties: {} }, () => "ok")] })
    const first = lowerAgent(agent)
    const second = lowerAgent(agent)
    expect(first).toEqual(second)
    expect(first).not.toBe(second)
    expect(first.capabilities).not.toBe(second.capabilities)
  })
})
