import { DynamicWorkflowScriptError, DynamicWorkflowVmExecutor } from "../src/workflow/dynamic-vm.js"
import type { DynamicWorkflowHost, DynamicWorkflowScript } from "../src/workflow/dynamic.js"

function host(calls: string[]): DynamicWorkflowHost {
  return {
    async runWorkflow(spec) {
      const node = spec.nodes[0]
      const prompt = typeof node.task === "string" ? node.task : node.task.goal
      calls.push(prompt)
      return {
        nodeOutcomes: [{ nodeId: node.nodeId!, status: "completed", output: { role: "assistant", content: `done:${prompt}` } }],
        outputs: { [node.nodeId!]: `done:${prompt}` },
      }
    },
  }
}

const script: DynamicWorkflowScript = {
  meta: { name: "vm-audit", description: "VM audit" },
  source: `
    return await workflow.phase("audit", async () => {
      workflow.log("starting", { source: "vm" });
      return (await workflow.agent("inspect", { label: "inspect" }))?.text;
    });
  `,
}

describe("DynamicWorkflowVmExecutor", () => {
  it("executes artifact source through the normal host workflow boundary", async () => {
    const calls: string[] = []
    const run = await new DynamicWorkflowVmExecutor(host(calls)).runScript(script, { runId: "vm-run" })
    expect(calls).toEqual(["inspect"])
    expect(run.value).toBe("done:inspect")
    expect(run.events.map(event => event.kind)).toContain("phase_completed")
  })

  it.each(["process", "require", "import", "eval", "Function"])("rejects forbidden capability %s", async capability => {
    await expect(new DynamicWorkflowVmExecutor(host([])).runScript({
      ...script,
      source: `return typeof ${capability}`,
    })).rejects.toMatchObject({ code: "DYNAMIC_WORKFLOW_SCRIPT" })
  })

  it("stops synchronous source that exceeds the VM timeout", async () => {
    await expect(new DynamicWorkflowVmExecutor(host([]), { timeoutMs: 10 }).runScript({
      ...script,
      source: "while (true) {}",
    })).rejects.toBeInstanceOf(DynamicWorkflowScriptError)
  })

  it("rejects oversized source before creating a VM", async () => {
    await expect(new DynamicWorkflowVmExecutor(host([]), { maxSourceBytes: 4 }).runScript({
      ...script,
      source: "return 1",
    })).rejects.toThrow(/maxSourceBytes/)
  })
})
