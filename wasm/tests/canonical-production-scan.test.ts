import { readFile } from "node:fs/promises"
import { resolve } from "node:path"

const runtime = (name: string) => resolve(process.cwd(), "src/runtime", name)
const forbidden = [
  /\.\s*step\(/,
  /\bkernelApply\(/,
  /\bkernelAction\(/,
  /\bkernelMaybeAction\(/,
  /\bnew\s+KernelRuntime\b/,
  /start_run|load_workflow|complete_run|ABI[-_ ]?v[12]/i,
  /CANONICAL_KERNEL_ABI_VERSION\s*=\s*3|["']abi_version["']\s*:\s*3/,
  /kernel-transaction-log|appendKernelGenesis|compareAndAppendKernelTransaction/,
  /submitWorkflowNodesToKernel|submitWorkflowToKernel|\bbootstrapWorkflow\b/,
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

  it("does not export direct step from the native binding", async () => {
    const source = await readFile(resolve(process.cwd(), "../crates/deepstrike-wasm/src/lib.rs"), "utf8")
    expect(source).not.toMatch(/pub fn step\s*\(/)
    expect(source).not.toMatch(/pub struct KernelRuntime\b/)

    const declarations = await readFile(resolve(process.cwd(), "src/wasm-kernel.d.ts"), "utf8")
    expect(declarations).not.toMatch(/export class KernelRuntime\b/)
  })
})
