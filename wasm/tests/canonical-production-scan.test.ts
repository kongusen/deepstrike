import { readFile } from "node:fs/promises"
import { resolve } from "node:path"

const runtime = (name: string) => resolve(process.cwd(), "src/runtime", name)
const forbidden = [
  /\.\s*step\(/,
  /\bkernelApply\(/,
  /\bkernelAction\(/,
  /\bkernelMaybeAction\(/,
  /\bnew\s+KernelRuntime\b/,
]

describe("Task 21 canonical production cutover", () => {
  it.each([
    "runner.ts",
    "canonical-kernel-step.ts",
    "sub-agent-orchestrator.ts",
  ])("does not use legacy direct-step APIs in %s", async file => {
    const source = await readFile(runtime(file), "utf8")
    for (const pattern of forbidden) {
      expect(source).not.toMatch(pattern)
    }
  })
})
