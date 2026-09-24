import { DynamicWorkflowProcessExecutor } from "../src/workflow/dynamic-process.js"
import { DynamicWorkflowController } from "../src/workflow/dynamic-controller.js"
import type { DynamicWorkflowHost, DynamicWorkflowScript } from "../src/workflow/dynamic.js"

const script: DynamicWorkflowScript = {
  meta: { name: "process-audit", description: "Process audit", phases: ["inspect"] },
  source: `
    return await workflow.phase("inspect", async () => {
      workflow.log("from-child");
      return (await workflow.agent("inspect", { label: "inspect" }))?.text;
    });
  `,
}

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

describe("DynamicWorkflowProcessExecutor", () => {
  it("executes untrusted source in a child process while keeping host lifecycle semantics", async () => {
    const calls: string[] = []
    const events: string[] = []
    const run = await new DynamicWorkflowProcessExecutor(host(calls)).runScript(script, {
      runId: "process-run",
      trust: "untrusted",
      onLifecycleEvent: event => events.push(event.kind),
    })

    expect(calls).toEqual(["inspect"])
    expect(run.value).toBe("done:inspect")
    expect(events).toContain("phase_started")
    expect(events.at(-1)).toBe("run_completed")
  })

  it("fails closed when process source requests a Node capability", async () => {
    await expect(new DynamicWorkflowProcessExecutor(host([])).runScript({
      ...script,
      source: "return typeof process",
    }, { trust: "untrusted" })).rejects.toThrow(/forbidden capability|failed/)
  })

  it("routes untrusted artifacts through the process executor in the controller", async () => {
    const controller = new DynamicWorkflowController()
    const runPromise = controller.start(script, { trust: "untrusted", runId: "controller-process" })
    const submission = await controller.nextSubmission()
    expect(submission?.spec.nodes[0]?.task).toBe("inspect")
    controller.completeSubmission(submission!.id, {
      nodeOutcomes: [{ nodeId: "inspect", status: "completed", output: { role: "assistant", content: "done" } }],
      outputs: { inspect: "done" },
    })
    await expect(runPromise).resolves.toMatchObject({ value: "done" })
  })
})
