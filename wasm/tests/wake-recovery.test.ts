/**
 * SessionLog-only wake must fail closed under canonical ABI v3 (Node wake-recovery parity).
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"
import { collectText } from "../src/runtime/index.js"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const provider: LLMProvider = {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "finished" }
  },
}

describe("RuntimeRunner wake recovery (wasm)", () => {
  it("does not continue from a SessionLog-only tool completion", async () => {
    const sessionLog = new InMemorySessionLog()
    const sessionId = "crash-test"
    await sessionLog.append(sessionId, {
      kind: "run_started", run_id: "r1", goal: "use ping", criteria: [],
    })
    await sessionLog.append(sessionId, {
      kind: "llm_completed",
      turn: 0,
      content: "",
      tool_calls: [{ id: "call_ping", name: "ping", arguments: "{}" }],
    })
    await sessionLog.append(sessionId, {
      kind: "tool_completed",
      turn: 0,
      results: [{ call_id: "call_ping", output: "pong", is_error: false }],
    })

    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 4,
    })

    await expect(collectText(runner.wake(sessionId))).rejects.toThrow(
      "restored canonical operation has no pending effect or terminal",
    )
  })

  it("rejects a terminal projection without a canonical journal", async () => {
    const sessionLog = new InMemorySessionLog()
    const sessionId = "projection-only-terminal"
    await sessionLog.append(sessionId, {
      kind: "run_started", run_id: "projection-only", goal: "done", criteria: [],
    })
    await sessionLog.append(sessionId, { kind: "run_terminal", termination: "completed" })
    const runner = new RuntimeRunner({ provider, sessionLog, maxTokens: 2_048, maxTurns: 2 })

    await expect(collectText(runner.wake(sessionId))).rejects.toThrow(
      "run_terminal projection has no canonical journal",
    )
  })

  it("drives a workflow spawn restored from the canonical journal", async () => {
    const sessionLog = new InMemorySessionLog()
    const sessionId = "workflow-wake"
    await sessionLog.append(sessionId, {
      kind: "run_started", run_id: "workflow-run", goal: "workflow", criteria: [],
    })

    const observations: Array<Record<string, unknown>> = []
    let terminal = false
    const restoredRuntime = {
      operationId: "wasm-operation-workflow-run",
      journal: sessionLog.kernelJournal,
      turn: () => 0,
      isTerminal: () => terminal,
      recoveryContentBytes: () => 32_768,
      preservedRefs: () => [] as string[],
      drainNewMessages: () => [] as Message[],
      drainHostObservations: () => observations.splice(0) as never,
      async restore() {},
      resumeAction: () => terminal
        ? {
            kind: "done" as const,
            effectId: "",
            result: { termination: "completed", turnsUsed: 0, totalTokensUsed: 0 },
          }
        : {
            kind: "spawn_workflow" as const,
            effectId: "spawn-restored",
            nodes: [{
              agent_id: "wf-node1",
              goal: "second",
              role: "plan",
              isolation: "shared",
              context_inheritance: "none",
              input_agent_ids: ["wf-node0"],
              dependency_outputs: { "wf-node0": "pre-crash dependency output" },
            }],
          },
      async applyHostEvent(event: Record<string, unknown>) {
        if (event.kind === "workflow_spawn_result") return null
        if (event.kind === "sub_agent_completed") {
          terminal = true
          observations.push({
            kind: "workflow_completed",
            node_outcomes: [{ node_id: "wf-node1", status: "completed", termination: "completed" }],
          })
          return null
        }
        throw new Error(`unexpected restored workflow event: ${String(event.kind)}`)
      },
    }

    const launched: string[] = []
    const goals: string[] = []
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 4,
      subAgentOrchestrator: {
        async run(context: { spec: { identity: { agentId: string }; goal: string } }) {
          launched.push(context.spec.identity.agentId)
          goals.push(context.spec.goal)
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
    ;(runner as never as { createCanonicalRuntime: () => Promise<unknown> }).createCanonicalRuntime =
      async () => restoredRuntime

    const events: StreamEvent[] = []
    for await (const event of runner.wake(sessionId)) events.push(event)

    expect(launched).toEqual(["wf-node1"])
    expect(goals[0]).toContain("pre-crash dependency output")
    expect(events.at(-1)).toMatchObject({ type: "done", status: "completed" })
  })

  it("does not call SessionLog repair from the production runner", () => {
    const source = readFileSync(join(process.cwd(), "src/runtime/runner.ts"), "utf8")
    expect(source).not.toContain("repairEventsForRecovery")
  })
})
