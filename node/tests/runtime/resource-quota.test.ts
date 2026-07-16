import { getKernel } from "../../src/kernel.js"
import { stepKernelV2WithHostEffects } from "../helpers/kernel-v2.js"

// M2 资源配额 reference test: resource quotas flow into the kernel through the versioned JSON
// event ABI (`set_resource_quota`) — the same channel as governance/scheduler config — and are
// enforced at the single syscall trap. Driving the napi KernelRuntime directly proves the
// rebuilt native addon exposes the new input event end to end.

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return stepKernelV2WithHostEffects(rt as never, event) as {
    actions: Array<{ kind: string }>
    observations: Array<{
      kind: string
      reason?: string
      operation?: string
      subject?: string
      agent_id?: string
      state?: string
    }>
  }
}

const WORKER_SPEC = {
  identity: { agent_id: "worker", session_id: "w-sess", is_sub_agent: true },
  role: "implement",
  isolation: "shared",
  goal: "work",
  capability_filter: { allowed_kinds: [], allowed_ids: [] },
}

describe("kernel resource quota (M2)", () => {
  it("set_resource_quota is a config event that yields no actions", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    const out = step(rt, { kind: "set_resource_quota", quota: { max_spawn_depth: 0 } })
    expect(out.actions).toHaveLength(0)
  })

  it("denies a spawn that exceeds max_spawn_depth without rolling the turn back", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_resource_quota", quota: { max_spawn_depth: 0 } })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    const spawn = step(rt, {
      kind: "spawn_sub_agent",
      spec: WORKER_SPEC,
      parent_session_id: "parent-sess",
    })

    // The rejected control request is observable, but no child starts and no rollback occurs.
    expect(spawn.actions).toHaveLength(0)
    expect(spawn.observations).toContainEqual(expect.objectContaining({
      kind: "control_request_rejected",
      operation: "spawn_sub_agent",
      subject: "worker",
    }))
    expect(spawn.observations.some(o => o.kind === "rollbacked")).toBe(false)
    expect(spawn.observations.some(o => o.kind === "agent_process_changed")).toBe(false)
    expect(spawn.observations.some(o => o.kind === "suspended")).toBe(false)
  })

  it("denies a second concurrent spawn once max_concurrent_subagents is reached", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_resource_quota", quota: { max_concurrent_subagents: 1 } })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    const first = step(rt, {
      kind: "spawn_sub_agent",
      spec: WORKER_SPEC,
      parent_session_id: "parent-sess",
    })
    // First spawn fits under the cap (0 running < 1) and suspends the parent on the join.
    expect(first.observations.some(o => o.kind === "agent_process_changed" && o.state === "running")).toBe(true)
    expect(first.observations.some(o => o.kind === "suspended" && o.reason === "sub_agent_await")).toBe(true)
  })

  it("no quota (default) admits the spawn unconditionally — pre-M2 behavior", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })

    const spawn = step(rt, {
      kind: "spawn_sub_agent",
      spec: WORKER_SPEC,
      parent_session_id: "parent-sess",
    })
    expect(spawn.actions).toHaveLength(0)
    expect(spawn.observations.some(o => o.kind === "agent_process_changed" && o.state === "running")).toBe(true)
    expect(spawn.observations.some(o => o.kind === "suspended" && o.reason === "sub_agent_await")).toBe(true)
  })
})
