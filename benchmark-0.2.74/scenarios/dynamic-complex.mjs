import { count, metric } from "../core/metrics.mjs"
import { createRunner } from "../core/runtime.mjs"

async function runComplex(sdk, options) {
  const calls = []
  const lifecycle = []
  const runner = createRunner(sdk, { onCall: context => calls.push(context.manifest.agent_id) })
  const result = await runner.runDynamicWorkflow(async ctx => {
    const seed = await ctx.phase("discover", async () => {
      const value = await ctx.agent("discover the initial system state", { label: "discover-seed" })
      ctx.log("seed discovered", { nodeId: value?.nodeId })
      return value
    })

    const probes = await ctx.phase("bounded-fanout", () => ctx.parallelAgents(
      ["api", "database", "queue", "cache", "search"],
      item => ({ prompt: `probe subsystem ${item}`, options: { label: `probe-${item}` } }),
    ))
    const probeTexts = probes.filter(Boolean).map(probe => probe.text)
    const branch = ctx.args.mode === "wide" && probeTexts.length === 5 ? "wide" : "fallback"
    const branchResult = await ctx.phase("conditional-branch", () => ctx.agent(
      `run ${branch} remediation path`, { label: `branch-${branch}` },
    ))

    const verification = await ctx.phase("serial-verification", () => ctx.pipeline(
      ["contract", "latency", "rollback"],
      check => ctx.agent(`verify ${check} after ${branchResult?.text ?? "branch"}`, { label: `verify-${check}` }),
    ))
    const aggregate = await ctx.phase("aggregate", () => ctx.agent(
      `aggregate ${[seed?.text, ...probeTexts, ...verification.map(item => item?.text)].join("|")}`,
      { label: "aggregate" },
    ))
    return {
      branch,
      seed: seed?.text,
      probes: probeTexts,
      verification: verification.map(item => item?.text),
      aggregate: aggregate?.text,
    }
  }, {
    runId: options.runId,
    sessionId: options.sessionId,
    args: { mode: "wide" },
    replayStore: options.replayStore,
    limits: { maxConcurrentAgents: 2, maxAgentsPerRun: 20, maxItemsPerBatch: 5 },
    approval: async request => {
      options.approvals.push({ runId: request.runId, mode: request.args.mode, maxAgents: request.limits.maxAgentsPerRun })
      return { approved: true, reason: "benchmark approval" }
    },
    onLifecycleEvent: event => lifecycle.push(event),
  })
  return { result, calls, lifecycle }
}

async function runPauseResume(sdk) {
  const control = new sdk.workflow.DynamicWorkflowControl()
  const lifecycle = []
  const runner = createRunner(sdk)
  const run = runner.runDynamicWorkflow(async ctx => {
    await ctx.agent("before pause", { label: "pause-before" })
    control.pause("benchmark pause")
    const pending = ctx.agent("after pause", { label: "pause-after" })
    setTimeout(() => control.resume(), 5)
    return (await pending)?.text
  }, {
    runId: "dynamic-complex-pause",
    sessionId: "dynamic-complex-pause",
    control,
    onLifecycleEvent: event => lifecycle.push(event),
  })
  const result = await run
  return { result, lifecycle }
}

export const dynamicComplex = {
  id: "dynamic-complex",
  description: "complex dynamic workflow phases, bounded fan-out, branch, pipeline, aggregate, pause/resume, and replay",
  variants: ["deterministic"],
  surfaces: ["advanced", "runtime", "workflow"],
  async run({ sdk }) {
    const replayStore = new sdk.workflow.InMemoryDynamicWorkflowReplayStore()
    const approvals = []
    const first = await runComplex(sdk, {
      runId: "dynamic-complex-main",
      sessionId: "dynamic-complex-first",
      replayStore,
      approvals,
    })
    const second = await runComplex(sdk, {
      runId: "dynamic-complex-main",
      sessionId: "dynamic-complex-replay",
      replayStore,
      approvals,
    })
    const paused = await runPauseResume(sdk)
    const firstAgentStarted = first.result.progress.agentsStarted
    const secondAgentReused = second.lifecycle.filter(event => event.kind === "agent_reused").length
    const phases = first.result.progress.phases.map(phase => phase.name)
    const pausedKinds = paused.lifecycle.map(event => event.kind)
    const passed = first.result.progress.status === "completed"
      && second.result.progress.status === "completed"
      && first.result.value.branch === "wide"
      && first.result.value.probes.length === 5
      && first.result.value.verification.length === 3
      && /^wf-node/.test(first.result.value.aggregate)
      && first.result.progress.agentsStarted === 11
      && second.result.progress.agentsReused === 11
      && first.calls.length === 11
      && second.calls.length === 0
      && approvals.length === 2
      && paused.result.progress.status === "completed"
      && pausedKinds.includes("pause_requested")
      && pausedKinds.includes("paused")
      && pausedKinds.includes("resumed")
    if (!passed) {
      throw new Error(JSON.stringify({
        first: first.result,
        second: second.result,
        firstCalls: first.calls,
        secondCalls: second.calls,
        approvals,
        paused: paused.result,
        pausedKinds,
      }))
    }
    return {
      metrics: {
        phases: count(phases.length),
        firstAgentsStarted: count(firstAgentStarted),
        replayAgentsReused: count(secondAgentReused),
        firstDispatches: count(first.calls.length),
        replayDispatches: count(second.calls.length),
        boundedFanoutWidth: metric(2, "agents"),
        pausedLifecycleEvents: count(pausedKinds.filter(kind => ["pause_requested", "paused", "resumed"].includes(kind)).length),
      },
      evidence: {
        phases,
        branch: first.result.value.branch,
        probeCount: first.result.value.probes.length,
        verificationCount: first.result.value.verification.length,
        firstLifecycle: first.lifecycle.map(event => event.kind),
        replayLifecycle: second.lifecycle.map(event => event.kind),
        pauseLifecycle: pausedKinds,
        approvals,
        replayAgentsReused: second.result.progress.agentsReused,
        replayDispatches: second.calls.length,
      },
    }
  },
}
