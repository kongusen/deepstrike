import { getKernel } from "../../src/kernel.js"

// Memory policy reference test: like governance / scheduler / resource-quota config, the memory
// policy flows into the kernel through the versioned JSON event ABI (`set_memory_policy`) and is
// enforced in-kernel. Driving the napi KernelRuntime directly proves the rebuilt native addon
// accepts the event end to end and treats it as pure config (no actions, replayable).

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return JSON.parse(rt.step(JSON.stringify({ version: 1, event }))) as {
    actions: Array<{ kind: string }>
    observations: Array<{ kind: string }>
  }
}

describe("kernel memory policy", () => {
  it("set_memory_policy is a config event that yields no actions", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    const out = step(rt, {
      kind: "set_memory_policy",
      memory_path: "/tmp/mem",
      stale_warning_days: 14,
      retrieval_top_k: 8,
      validation_enabled: false,
    })
    expect(out.actions).toHaveLength(0)
  })

  it("accepts a partial policy, defaulting omitted fields in-kernel", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    const out = step(rt, { kind: "set_memory_policy", retrieval_top_k: 3 })
    expect(out.actions).toHaveLength(0)
  })

  it("does not disturb a subsequent run start", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_memory_policy", validation_enabled: false })
    const run = step(rt, { kind: "start_run", task: { goal: "work", criteria: [] } })
    // A fresh run starts by reasoning — the policy event is config-only and non-disruptive.
    expect(run.actions[0]?.kind).toBe("call_provider")
  })

  // ── Enforcement: the kernel honors the policy at the WriteMemory / QueryMemory traps ──

  const FORBIDDEN_WRITE = {
    kind: "write_memory",
    memory: { metadata: { name: "note", description: "desc" }, content: "代码模式: foo" },
  }

  it("validation_enabled:false admits any structurally valid write", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_memory_policy", validation_enabled: false })
    const out = step(rt, FORBIDDEN_WRITE)
    expect(out.observations.some(o => o.kind === "memory_written")).toBe(true)
    expect(out.observations.some(o => o.kind === "memory_validation_failed")).toBe(false)
  })

  it("default (no policy) accepts content hosts have not forbidden", () => {
    // P13: no baked-in forbidden patterns — content judgment belongs to hosts/models.
    // Structural validation (name/description/size) still applies.
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    const out = step(rt, FORBIDDEN_WRITE)
    expect(out.observations.some(o => o.kind === "memory_written")).toBe(true)
  })

  it("max_content_bytes override rejects an oversized write", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_memory_policy", max_content_bytes: 8 })
    const out = step(rt, {
      kind: "write_memory",
      memory: { metadata: { name: "note", description: "desc" }, content: "way more than eight bytes" },
    })
    expect(out.observations.some(o => o.kind === "memory_validation_failed")).toBe(true)
  })

  it("retrieval_top_k caps the emitted requested_k", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_memory_policy", retrieval_top_k: 3 })
    const out = JSON.parse(
      rt.step(
        JSON.stringify({
          version: 1,
          event: { kind: "query_memory", query: { current_context: "ctx", top_k: 50 } },
        }),
      ),
    ) as { observations: Array<{ kind: string; requested_k?: number }> }
    const queried = out.observations.find(o => o.kind === "memory_queried")
    expect(queried?.requested_k).toBe(3)
  })
})
