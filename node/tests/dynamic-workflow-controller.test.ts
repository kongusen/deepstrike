import { DynamicWorkflowController } from "../src/workflow/dynamic-controller.js"
import { DynamicWorkflowApprovalError } from "../src/workflow/dynamic.js"
import type { WorkflowOutcome, WorkflowSpec } from "../src/types/agent.js"

function outcome(spec: WorkflowSpec, suffix: string): WorkflowOutcome {
  return {
    nodeOutcomes: spec.nodes.map(node => ({
      nodeId: node.nodeId!,
      status: "completed" as const,
      output: { role: "assistant" as const, content: `${node.task as string}:${suffix}` },
    })),
    outputs: Object.fromEntries(spec.nodes.map(node => [node.nodeId!, `${node.task as string}:${suffix}`])),
  }
}

describe("DynamicWorkflowController", () => {
  it("hands script submissions to the driver and resumes in submission order", async () => {
    const controller = new DynamicWorkflowController()
    const runPromise = controller.start(async ctx => [
      (await ctx.agent("first", { label: "first" }))?.text,
      (await ctx.agent("second", { label: "second" }))?.text,
    ])

    const first = await controller.nextSubmission()
    expect(first?.spec.nodes[0]?.nodeId).toBe("first")
    controller.completeSubmission(first!.id, outcome(first!.spec, "ok"))

    const second = await controller.nextSubmission()
    expect(second?.spec.nodes[0]?.nodeId).toBe("second")
    controller.completeSubmission(second!.id, outcome(second!.spec, "ok"))

    await expect(runPromise).resolves.toMatchObject({ value: ["first:ok", "second:ok"] })
    await expect(controller.nextSubmission()).resolves.toBeUndefined()
    expect(controller.isFinished).toBe(true)
  })

  it("supports concurrent submissions and preserves parallel result order", async () => {
    const controller = new DynamicWorkflowController()
    const runPromise = controller.start(ctx =>
      ctx.parallel(["a", "b"], item => ctx.agent(`task:${item}`, { label: item })),
    )

    const first = await controller.nextSubmission()
    const second = await controller.nextSubmission()
    expect([first?.spec.nodes[0]?.nodeId, second?.spec.nodes[0]?.nodeId]).toEqual(["a", "b"])

    controller.completeSubmission(second!.id, outcome(second!.spec, "done"))
    controller.completeSubmission(first!.id, outcome(first!.spec, "done"))

    const run = await runPromise
    expect(run.value.map(result => result?.text)).toEqual(["task:a:done", "task:b:done"])
  })

  it("propagates a driver failure to the dynamic script", async () => {
    const controller = new DynamicWorkflowController()
    const runPromise = controller.start(ctx => ctx.agent("will-fail"))
    const submission = await controller.nextSubmission()

    controller.failSubmission(submission!.id, new Error("kernel unavailable"))

    await expect(runPromise).rejects.toThrow("kernel unavailable")
    await expect(controller.nextSubmission()).resolves.toBeUndefined()
  })

  it("requires approval before creating a submission and emits ordered lifecycle events", async () => {
    const controller = new DynamicWorkflowController()
    const events: string[] = []
    const runPromise = controller.start(ctx => ctx.agent("must not run"), {
      approval: () => false,
      onLifecycleEvent: event => events.push(event.kind),
    })

    await expect(runPromise).rejects.toBeInstanceOf(DynamicWorkflowApprovalError)
    await expect(controller.nextSubmission()).resolves.toBeUndefined()
    expect(events).toEqual([
      "run_started",
      "approval_requested",
      "approval_resolved",
      "run_cancelled",
    ])
  })
})
