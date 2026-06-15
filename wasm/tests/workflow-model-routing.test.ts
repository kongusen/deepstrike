import { workflowNodeToSpec } from "../src/runtime/types/agent.js"
import type { WorkflowSpawnInfo } from "../src/runtime/types/agent.js"
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

describe("M1/G3 model routing (wasm)", () => {
  it("workflowNodeToSpec carries model_hint → spec.modelHint (and omits when absent)", () => {
    expect(workflowNodeToSpec(node({ model_hint: "opus" }), "sess").modelHint).toBe("opus")
    expect(workflowNodeToSpec(node(), "sess").modelHint).toBeUndefined()
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

    expect(resolveProvider(opts, "opus")).toBe(opus)
    expect(seen).toEqual(["opus"])
    expect(resolveProvider(opts, "unknown")).toBe(base)
    expect(resolveProvider(opts, undefined)).toBe(base)
    expect(resolveProvider({ provider: base } as RuntimeOptions, "opus")).toBe(base)
  })
})
