import { readFileSync, existsSync } from "node:fs"
import { join } from "node:path"
import { getKernel } from "../src/kernel.js"

function getFixturesDir(): string {
  const path1 = join(process.cwd(), "tests/fixtures/abi")
  if (existsSync(path1)) return path1
  const path2 = join(process.cwd(), "../tests/fixtures/abi")
  if (existsSync(path2)) return path2
  const path3 = join(process.cwd(), "../../tests/fixtures/abi")
  if (existsSync(path3)) return path3
  throw new Error("Could not locate tests/fixtures/abi")
}

describe("Golden ABI Fixtures", () => {
  let fixturesDir: string
  let KernelRuntime: any

  beforeAll(() => {
    fixturesDir = getFixturesDir()
    KernelRuntime = getKernel().KernelRuntime
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
})
