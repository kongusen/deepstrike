import { getKernel } from "../../src/kernel.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { CanonicalRunnerRuntime } from "../../src/runtime/canonical-kernel-step.js"
import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { tool } from "../../src/tools/index.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../../src/types.js"

class FinishAfterToolProvider implements LLMProvider {
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }

  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    if (context.turns.some(turn => turn.role === "tool")) {
      yield { type: "text_delta", delta: "resumed-finish" }
      return
    }
    yield { type: "text_delta", delta: "fresh-finish" }
  }
}

function runtime(log: InMemorySessionLog, runId: string): CanonicalRunnerRuntime {
  return new CanonicalRunnerRuntime(
    new (getKernel().CanonicalKernel)(),
    log.kernelJournal,
    `node-operation-${runId}`,
    { maxContextTokens: 8_000, maxTurns: 8 },
  )
}

describe("Task 19 — Node canonical host cutover", () => {
  it("uses KernelJournal record bytes and never calls the legacy SessionLog transaction face", async () => {
    const log = new InMemorySessionLog()
    expect(log).not.toHaveProperty("appendKernelGenesis")
    expect(log).not.toHaveProperty("compareAndAppendKernelTransaction")
    const runner = new RuntimeRunner({
      provider: new FinishAfterToolProvider(),
      sessionLog: log,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8_000,
      maxTurns: 4,
    })

    expect(await collectText(runner.run({ sessionId: "canonical-only", goal: "finish" })))
      .toBe("fresh-finish")

    const started = (await log.read("canonical-only"))
      .find(entry => entry.event.kind === "run_started")?.event
    expect(started?.kind).toBe("run_started")
    const operationId = `node-operation-${started && "run_id" in started ? started.run_id : ""}`
    const entries = await log.kernelJournal.readFrom(operationId)
    expect(entries.length).toBeGreaterThanOrEqual(3)

    const genesis = JSON.parse(Buffer.from(entries[0].record_bytes).toString("utf8")) as {
      canonical_input: { data: string }
    }
    expect(genesis).not.toHaveProperty("abi_version")
    const canonicalInput = JSON.parse(Buffer.from(genesis.canonical_input.data, "base64").toString("utf8"))
    expect(canonicalInput.input.kind).toBe("configure_operation")
    expect(JSON.stringify(canonicalInput)).not.toContain("session_id")
  })

  it("restores a pending tool effect from journal bytes and executes it exactly once", async () => {
    const log = new InMemorySessionLog()
    const runId = "crash-agent-1"
    const sessionId = "canonical-agent-wake"
    await log.append(sessionId, {
      kind: "run_started",
      run_id: runId,
      goal: "ping then finish",
      criteria: [],
    })

    const beforeCrash = runtime(log, runId)
    await beforeCrash.applyHostEvent({
      kind: "set_tools",
      tools: [{ name: "ping", description: "ping", parameters: { type: "object" } }],
    })
    const first = await beforeCrash.startAgent(
      { goal: "ping then finish", criteria: [] },
      { exposure_baseline: ["ping"] },
    )
    expect(first?.kind).toBe("call_provider")
    const pending = await beforeCrash.applyHostEvent({
      kind: "provider_result",
      effect_id: first?.effectId,
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "call-ping", name: "ping", arguments: {} }],
      },
      stop_reason: "tool_use",
    })
    expect(pending?.kind).toBe("execute_tool")

    let executions = 0
    const plane = new LocalExecutionPlane()
    plane.register(tool("ping", "ping", { type: "object" }, () => {
      executions += 1
      return "pong"
    }))
    const afterRestart = new RuntimeRunner({
      provider: new FinishAfterToolProvider(),
      sessionLog: log,
      executionPlane: plane,
      maxTokens: 8_000,
      maxTurns: 8,
    })

    expect(await collectText(afterRestart.wake(sessionId))).toBe("resumed-finish")
    expect(executions).toBe(1)
  })

  it("does not let a SessionLog terminal projection suppress a live canonical operation", async () => {
    const log = new InMemorySessionLog()
    const runId = "live-kernel-stale-audit"
    const sessionId = "canonical-authority"
    await log.append(sessionId, {
      kind: "run_started",
      run_id: runId,
      goal: "ping then finish",
      criteria: [],
    })

    const beforeCrash = runtime(log, runId)
    await beforeCrash.applyHostEvent({
      kind: "set_tools",
      tools: [{ name: "ping", description: "ping", parameters: { type: "object" } }],
    })
    const first = await beforeCrash.startAgent(
      { goal: "ping then finish", criteria: [] },
      { exposure_baseline: ["ping"] },
    )
    const pending = await beforeCrash.applyHostEvent({
      kind: "provider_result",
      effect_id: first?.effectId,
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "call-ping", name: "ping", arguments: {} }],
      },
      stop_reason: "tool_use",
    })
    expect(pending?.kind).toBe("execute_tool")
    await log.append(sessionId, {
      kind: "run_terminal",
      reason: "completed",
      turns_used: 1,
      total_tokens: 0,
    })

    let executions = 0
    const plane = new LocalExecutionPlane()
    plane.register(tool("ping", "ping", { type: "object" }, () => {
      executions += 1
      return "pong"
    }))
    const afterRestart = new RuntimeRunner({
      provider: new FinishAfterToolProvider(),
      sessionLog: log,
      executionPlane: plane,
      maxTokens: 8_000,
      maxTurns: 8,
    })

    expect(await collectText(afterRestart.wake(sessionId))).toBe("resumed-finish")
    expect(executions).toBe(1)
  })

  it("run() also refuses to orphan a live journal behind a forged SessionLog terminal", async () => {
    const log = new InMemorySessionLog()
    const runId = "live-kernel-run-authority"
    const sessionId = "canonical-run-authority"
    await log.append(sessionId, {
      kind: "run_started",
      run_id: runId,
      goal: "ping then finish",
      criteria: [],
    })

    const beforeCrash = runtime(log, runId)
    await beforeCrash.applyHostEvent({
      kind: "set_tools",
      tools: [{ name: "ping", description: "ping", parameters: { type: "object" } }],
    })
    const first = await beforeCrash.startAgent(
      { goal: "ping then finish", criteria: [] },
      { exposure_baseline: ["ping"] },
    )
    const pending = await beforeCrash.applyHostEvent({
      kind: "provider_result",
      effect_id: first?.effectId,
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "call-ping", name: "ping", arguments: {} }],
      },
      stop_reason: "tool_use",
    })
    expect(pending?.kind).toBe("execute_tool")
    await log.append(sessionId, {
      kind: "run_terminal",
      reason: "completed",
      turns_used: 1,
      total_tokens: 0,
    })

    let executions = 0
    const plane = new LocalExecutionPlane()
    plane.register(tool("ping", "ping", { type: "object" }, () => {
      executions += 1
      return "pong"
    }))
    const afterRestart = new RuntimeRunner({
      provider: new FinishAfterToolProvider(),
      sessionLog: log,
      executionPlane: plane,
      maxTokens: 8_000,
      maxTurns: 8,
    })

    expect(await collectText(afterRestart.run({
      sessionId,
      goal: "must resume, not mint a new operation",
    }))).toBe("resumed-finish")
    expect(executions).toBe(1)

    const starts = (await log.read(sessionId)).filter(entry => entry.event.kind === "run_started")
    expect(starts).toHaveLength(1)
    expect(starts[0].event).toMatchObject({ run_id: runId })
    expect(await log.kernelJournal.head(`node-operation-${runId}`)).toBeDefined()
  })

  it("restores a downstream workflow spawn with its pre-crash dependency output", async () => {
    const log = new InMemorySessionLog()
    const runId = "crash-workflow-1"
    const sessionId = "canonical-workflow-wake"
    await log.append(sessionId, {
      kind: "run_started",
      run_id: runId,
      goal: "workflow:2 nodes",
      criteria: [],
    })

    const beforeCrash = runtime(log, runId)
    const pending = await beforeCrash.startWorkflow({
      nodes: [
        { task: { goal: "first" }, role: "explore", isolation: "shared", context_inheritance: "none" },
        {
          task: { goal: "second" },
          role: "plan",
          isolation: "shared",
          context_inheritance: "none",
          depends_on: [0],
        },
      ],
    })
    expect(pending?.kind).toBe("spawn_workflow")
    await beforeCrash.applyHostEvent({
      kind: "workflow_spawn_result",
      effect_id: pending?.effectId,
      started_agent_ids: ["wf-node0"],
      failures: [],
    })
    const downstream = await beforeCrash.applyHostEvent({
      kind: "sub_agent_completed",
      result: {
        agent_id: "wf-node0",
        result: {
          termination: "completed",
          final_message: {
            role: "assistant",
            content: "pre-crash dependency output",
            tool_calls: [],
          },
          turns_used: 1,
          total_tokens_used: 1,
        },
      },
    })
    expect(downstream?.kind).toBe("spawn_workflow")

    const launched: string[] = []
    const goals: string[] = []
    const afterRestart = new RuntimeRunner({
      provider: new FinishAfterToolProvider(),
      sessionLog: log,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8_000,
      maxTurns: 8,
      subAgentOrchestrator: {
        async run(ctx: { manifest: { agent_id: string }; spec: { goal: string } }) {
          launched.push(ctx.manifest.agent_id)
          goals.push(ctx.spec.goal)
          return {
            agentId: ctx.manifest.agent_id,
            result: {
              termination: "completed",
              finalMessage: { role: "assistant", content: "done", toolCalls: [] },
              turnsUsed: 1,
              totalTokensUsed: 1,
            },
          }
        },
      } as never,
    })

    const events: StreamEvent[] = []
    for await (const event of afterRestart.wake(sessionId)) events.push(event)
    expect(launched).toEqual(["wf-node1"])
    expect(goals[0]).toContain("pre-crash dependency output")
    expect(events.at(-1)).toMatchObject({ type: "done", status: "completed" })
  })
})
