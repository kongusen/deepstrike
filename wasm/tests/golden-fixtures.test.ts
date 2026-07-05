import { readFileSync, existsSync } from "node:fs"
import { join } from "node:path"
import { KernelRuntime } from "@deepstrike/wasm-kernel"

function getFixturesDir(): string {
  const path1 = join(process.cwd(), "tests/fixtures/abi")
  if (existsSync(path1)) return path1
  const path2 = join(process.cwd(), "../tests/fixtures/abi")
  if (existsSync(path2)) return path2
  const path3 = join(process.cwd(), "../../tests/fixtures/abi")
  if (existsSync(path3)) return path3
  throw new Error("Could not locate tests/fixtures/abi")
}

describe("WASM Golden ABI Fixtures", () => {
  let fixturesDir: string

  beforeAll(() => {
    fixturesDir = getFixturesDir()
  })

  it("successfully steps with input_start_run.json", () => {
    const kernel = new KernelRuntime({ maxTokens: 2048 })
    const inputJson = readFileSync(join(fixturesDir, "input_start_run.json"), "utf8")
    
    const stepJson = kernel.step(inputJson)
    expect(stepJson).toBeDefined()
    
    const step = JSON.parse(stepJson)
    expect(step.version).toBe(1)
    expect(step.actions).toBeDefined()
    expect(step.actions.length).toBeGreaterThan(0)
    expect(step.actions[0].kind).toBe("call_provider")
  })

  it("successfully steps with input_tool_results.json after starting a run", () => {
    const kernel = new KernelRuntime({ maxTokens: 2048 })
    const startJson = readFileSync(join(fixturesDir, "input_start_run.json"), "utf8")
    kernel.step(startJson)

    // Feed a tool response mock
    const inputJson = readFileSync(join(fixturesDir, "input_tool_results.json"), "utf8")
    const stepJson = kernel.step(inputJson)
    expect(stepJson).toBeDefined()

    const step = JSON.parse(stepJson)
    expect(step.version).toBe(1)
    expect(step.actions).toBeDefined()
  })

  it("input_push_artifact.json produces no actions and no observations", () => {
    const kernel = new KernelRuntime({ maxTokens: 2048 })
    const inputJson = readFileSync(join(fixturesDir, "input_push_artifact.json"), "utf8")

    const stepJson = kernel.step(inputJson)
    const step = JSON.parse(stepJson)
    expect(step.version).toBe(1)
    expect(step.actions).toHaveLength(0)
    expect(step.observations).toHaveLength(0)
  })

  it("input_spawn_sub_agent.json emits agent_process_changed after start_run", () => {
    const kernel = new KernelRuntime({ maxTokens: 2048 })
    kernel.step(readFileSync(join(fixturesDir, "input_start_run.json"), "utf8"))

    const step = JSON.parse(kernel.step(readFileSync(join(fixturesDir, "input_spawn_sub_agent.json"), "utf8")))
    expect(step.version).toBe(1)
    expect(step.actions).toHaveLength(0)
    const spawned = step.observations.find((o: { kind: string }) => o.kind === "agent_process_changed")
    expect(spawned).toBeDefined()
    expect(spawned.agent_id).toBe("worker")
    expect(spawned.state).toBe("running")
  })

  it.each([
    ["observation_agent_process_changed.json", { kind: "agent_process_changed", agent_id: "worker", state: "running" }],
    ["observation_checkpoint_taken.json",    { kind: "checkpoint_taken",    turn: 2, history_len: 4 }],
    ["observation_renewed.json",             { kind: "renewed",             sprint: 2 }],
    ["observation_rollbacked.json",          { kind: "rollbacked",          turn: 2, checkpoint_history_len: 3 }],
    ["observation_capability_changed.json",  { kind: "capability_changed",  turn: 1, capability_id: "write_file" }],
    ["observation_milestone_advanced.json",  { kind: "milestone_advanced",  turn: 3, phase_id: "phase-1" }],
    ["observation_milestone_blocked.json",   { kind: "milestone_blocked",   turn: 3, phase_id: "phase-1" }],
  ])("%s round-trips required fields", (filename, expected) => {
    const raw = JSON.parse(readFileSync(join(fixturesDir, filename as string), "utf8"))
    for (const [k, v] of Object.entries(expected as Record<string, unknown>)) {
      expect(raw[k]).toEqual(v)
    }
  })
})
