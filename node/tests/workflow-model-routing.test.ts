import { workflowNodeToSpec } from "../src/types/agent.js"
import type { WorkflowSpawnInfo } from "../src/types/agent.js"
import { resolveProvider } from "../src/runtime/sub-agent-orchestrator.js"
import type { RuntimeOptions } from "../src/runtime/runner.js"

const node = (over: Partial<WorkflowSpawnInfo> = {}): WorkflowSpawnInfo => ({
  agent_id: "wf-node0",
  goal: "g",
  role: "plan",
  isolation: "shared",
  context_inheritance: "none",
  ...over,
})

describe("M1/G3 model routing", () => {
  it("workflowNodeToSpec carries model_hint → spec.modelHint (and omits when absent)", () => {
    expect(workflowNodeToSpec(node({ model_hint: "opus" }), "sess").modelHint).toBe("opus")
    expect(workflowNodeToSpec(node(), "sess").modelHint).toBeUndefined()
  })

  it("workflowNodeToSpec carries token_budget → spec.tokenBudget (M4/G5)", () => {
    expect(workflowNodeToSpec(node({ token_budget: 10000 }), "sess").tokenBudget).toBe(10000)
    expect(workflowNodeToSpec(node(), "sess").tokenBudget).toBeUndefined()
  })

  it("resolveProvider routes via providerFor with the right hint, else falls back to provider", () => {
    const base = { id: "base" } as unknown as RuntimeOptions["provider"]
    const opus = { id: "opus" } as unknown as RuntimeOptions["provider"]
    const seen: string[] = []
    const opts = {
      provider: base,
      providerFor: (h: string) => {
        seen.push(h)
        return h === "opus" ? opus : undefined
      },
    } as RuntimeOptions

    expect(resolveProvider(opts, "opus")).toBe(opus) // hook resolves → routed
    expect(seen).toEqual(["opus"]) // hook was called with the hint
    expect(resolveProvider(opts, "unknown")).toBe(base) // hook returns undefined → fallback
    expect(resolveProvider(opts, undefined)).toBe(base) // no hint → fallback
    expect(resolveProvider({ provider: base } as RuntimeOptions, "opus")).toBe(base) // no hook → fallback
  })
})
