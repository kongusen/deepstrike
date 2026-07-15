import { getKernel } from "../../src/kernel.js"
import { stepKernelV2WithHostEffects } from "../helpers/kernel-v2.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return stepKernelV2WithHostEffects(rt as never, event) as {
    actions: Array<Record<string, unknown>>
    observations: Array<{ kind: string; reason?: string; agent_id?: string; state?: string }>
  }
}

describe("kernel process table (Phase 3)", () => {
  it("spawn suspends parent until sub_agent_completed", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })
    const spawn = step(rt, {
      kind: "spawn_sub_agent",
      spec: {
        identity: { agent_id: "worker", session_id: "w-sess", is_sub_agent: true },
        role: "implement",
        isolation: "shared",
        goal: "work",
        capability_filter: { allowed_kinds: [], allowed_ids: [] },
      },
      parent_session_id: "parent-sess",
    })
    expect(spawn.actions).toHaveLength(0)
    expect(spawn.observations.some(o => o.kind === "agent_process_changed" && o.state === "running")).toBe(true)
    expect(spawn.observations.some(o => o.kind === "suspended" && o.reason === "sub_agent_await")).toBe(true)

    const done = step(rt, {
      kind: "sub_agent_completed",
      result: {
        agent_id: "worker",
        result: {
          termination: "completed",
          final_message: { role: "assistant", content: "ok", tool_calls: [] },
          turns_used: 1,
          total_tokens_used: 1,
        },
      },
    })
    expect(done.actions[0]?.kind).toBe("call_provider")
    expect(done.observations.some(o => o.kind === "agent_process_changed" && o.state === "joined")).toBe(true)
    expect(done.observations.some(o => o.kind === "resumed")).toBe(true)
  })
})
