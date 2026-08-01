import { InMemorySessionLog, LocalExecutionPlane, RuntimeRunner, runFanout } from "../src/runtime/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

describe("Task 21 standalone workflow cutover", () => {
  it("runFanout executes the public context-inheritance template instead of returning empty success", async () => {
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "facade-output", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "facade-output" }
      },
    }

    const outcome = await runFanout({ provider, tasks: ["worker"], synthesize: "merge", maxTurns: 1 })

    expect(Object.keys(outcome.outputs).sort()).toEqual(["wf-node0", "wf-node1"])
    expect(outcome.synthesis).toBe("facade-output")
  })

  it("uses one durable operation identity and commits the workflow terminal", async () => {
    const sessionLog = new InMemorySessionLog()
    const sessionId = "standalone-workflow"
    const observations: Array<Record<string, unknown>> = []
    let createdRunId = ""
    let terminal = false

    const runtime = {
      operationId: "",
      turn: () => 0,
      isTerminal: () => terminal,
      preservedRefs: () => [] as string[],
      localSubagentsSpawned: () => 1,
      drainHostObservations: () => observations.splice(0) as never,
      async startWorkflow(_spec: Record<string, unknown>) {
        return {
          kind: "spawn_workflow" as const,
          effectId: "workflow-spawn",
          nodes: [{
            agent_id: "wf-node0",
            goal: "finish",
            role: "implement",
            isolation: "shared",
            context_inheritance: "none",
            input_agent_ids: [],
          }],
        }
      },
      resumeAction() {
        return terminal
          ? {
              kind: "done" as const,
              effectId: "",
              result: { termination: "completed", turnsUsed: 0, totalTokensUsed: 0 },
            }
          : null
      },
      async applyHostEvent(event: Record<string, unknown>) {
        if (event.kind === "workflow_spawn_result") return null
        if (event.kind === "sub_agent_completed") {
          terminal = true
          observations.push({
            kind: "workflow_completed",
            node_outcomes: [{ node_id: "wf-node0", status: "completed", termination: "completed" }],
          })
          return null
        }
        return null
      },
    }

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "unused" }
      },
    }
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      subAgentOrchestrator: {
        async run(context: { spec: { identity: { agentId: string } } }) {
          return {
            agentId: context.spec.identity.agentId,
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
    ;(runner as never as {
      createCanonicalRuntime: (runId: string) => Promise<unknown>
    }).createCanonicalRuntime = async runId => {
      createdRunId = runId
      runtime.operationId = `wasm-operation-${runId}`
      return runtime
    }

    const outcome = await runner.runWorkflow(
      { nodes: [{ task: "finish", role: "implement" }] },
      { sessionId },
    )

    const started = (await sessionLog.read(sessionId))
      .find(entry => entry.event.kind === "run_started")?.event
    expect(started).toMatchObject({ kind: "run_started", run_id: createdRunId })
    expect(outcome.nodeOutcomes).toHaveLength(1)
    expect(terminal).toBe(true)
  })
})
