import { count, metric } from "../core/metrics.mjs"
import { createRunner } from "../core/runtime.mjs"

async function runWorkflowVariant(sdk, variant) {
  const calls = []
  const sessionLog = new sdk.advanced.InMemorySessionLog()
  const runner = createRunner(sdk, {
    sessionLog,
    onCall: context => calls.push(context.manifest.agent_id),
    schedulerPolicy: variant === "critical-path" ? { criticalPathWeight: 2, fanoutWeight: 1, ageWeight: 0, tokenCostWeight: 0 } : undefined,
  })
  const outcome = await runner.runWorkflow({
    nodes: [
      { task: "inspect", role: "explore", nodeId: "inspect" },
      { task: "verify", role: "verify", nodeId: "verify" },
      { task: "summarize", role: "plan", nodeId: "summarize", dependsOn: [0, 1] },
    ],
  }, { sessionId: `workflow-${variant}` })
  const events = await sessionLog.read(`workflow-${variant}`)
  const completed = outcome.nodeOutcomes.filter(node => node.status === "completed").length
  return {
    metrics: {
      nodes: count(outcome.nodeOutcomes.length),
      completedNodes: count(completed),
      hostDispatches: count(calls.length),
      sessionEvents: count(events.length),
      outputChars: metric(Object.values(outcome.outputs).join("").length, "chars"),
    },
    evidence: {
      variant,
      dispatchOrder: calls,
      statuses: outcome.nodeOutcomes.map(node => ({ nodeId: node.nodeId, status: node.status })),
      outputs: outcome.outputs,
    },
  }
}

async function runDynamicVariant(sdk, variant) {
  const calls = []
  const replayStore = new sdk.workflow.InMemoryDynamicWorkflowReplayStore()
  const program = async ctx => {
    const first = await ctx.agent("first", { label: "first" })
    const rest = await ctx.parallel(["a", "b"], item => ctx.agent(`item:${item}`, { label: item }))
    return [first?.text, ...rest.map(result => result?.text)]
  }
  const run = async (runId, sessionId) => {
    const runner = createRunner(sdk, {
      onCall: context => calls.push(context.manifest.agent_id),
    })
    return runner.runDynamicWorkflow(program, { runId, sessionId, replayStore })
  }
  const first = await run("dynamic-benchmark", `dynamic-${variant}-first`)
  const second = await run("dynamic-benchmark", `dynamic-${variant}-second`)
  if (first.progress.status !== "completed" || second.progress.status !== "completed") throw new Error("dynamic workflow did not complete")
  if (second.progress.agentsReused !== 3 || calls.length !== 3) throw new Error("dynamic workflow replay did not reuse all completed invocations")
  return {
    metrics: {
      firstAgentsStarted: count(first.progress.agentsStarted),
      replayAgentsReused: count(second.progress.agentsReused),
      replayAgentsStarted: count(second.progress.agentsStarted),
      lifecycleEvents: count(second.events.length),
    },
    evidence: {
      firstValue: first.value,
      replayValue: second.value,
      replayEvents: second.events.map(event => event.kind),
      providerDispatches: calls,
    },
  }
}

export const workflow = {
  id: "workflow",
  description: "public RuntimeRunner workflow and dynamic workflow replay",
  variants: ["default", "critical-path"],
  surfaces: ["advanced", "runtime", "workflow"],
  async run({ sdk, variant = "default" }) {
    const staticRun = await runWorkflowVariant(sdk, variant)
    const dynamicRun = await runDynamicVariant(sdk, variant)
    return {
      metrics: { static: staticRun.metrics, dynamic: dynamicRun.metrics },
      evidence: { static: staticRun.evidence, dynamic: dynamicRun.evidence },
    }
  },
}
