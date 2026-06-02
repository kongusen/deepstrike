import { getKernel } from "../../src/kernel.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return JSON.parse(rt.step(JSON.stringify({ version: 1, event }))) as {
    actions: unknown[]
    observations: Array<{ kind: string; tool?: string; tier_hint?: string }>
  }
}

describe("memory paging (Phase 4)", () => {
  it("page_in adds knowledge entries", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_memory_enabled", enabled: true })
    step(rt, {
      kind: "page_in",
      entries: [{ content: "[memory] recalled fact", tokens: 8, source: "memory" }],
    })
    const rendered = rt.render() as { system_knowledge?: string; systemKnowledge?: string }
    const text = rendered.system_knowledge ?? rendered.systemKnowledge ?? ""
    expect(text).toContain("recalled fact")
  })

  it("memory tool proposal emits page_in_requested", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_memory_enabled", enabled: true })
    step(rt, { kind: "start_run", task: { goal: "recall", criteria: [] } })
    const s = step(rt, {
      kind: "provider_result",
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{
          id: "m1",
          name: "memory",
          arguments: { query: "prior session", top_k: 3 },
        }],
      },
    })
    expect(s.observations.some(o => o.kind === "page_in_requested" && o.tool === "memory")).toBe(true)
    expect(s.actions[0]).toMatchObject({ kind: "execute_tool" })
  })
})
