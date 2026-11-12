// Plan task 9.1 · notes the model writes with the kernel's `note` tool: offered only with a notes
// budget, consumed by the kernel (never executed by the host), rendered in the task state from the
// next turn on, and counted in the lifecycle report.
import { collectText, RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import { contextPolicy, normalizeContextPolicy } from "../src/runtime/context-policy.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

function scripted() {
  const offered: string[][] = []
  const states: string[] = []
  let calls = 0
  const provider: LLMProvider = {
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(context: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
      calls += 1
      offered.push(tools.map(t => t.name))
      states.push(JSON.stringify(context.stateTurn ?? ""))
      if (calls === 1) {
        yield { type: "tool_call", id: "c1", name: "read", arguments: { path: "logs/a.log" } }
        return
      }
      if (calls === 2 && tools.some(t => t.name === "note")) {
        yield {
          type: "tool_call",
          id: "c2",
          name: "note",
          arguments: { title: "Tally", kind: "state", body: "3 qualifying lines in logs/a.log" },
        }
        return
      }
      yield { type: "text_delta", delta: "3" }
    },
  }
  return { provider, offered, states }
}

const read = tool("read", "read a file", { type: "object", properties: { path: { type: "string" } } }, () => "ERROR x\nERROR y\nERROR z")

function runner(provider: LLMProvider, notesBudgetTokens?: number) {
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  plane.register(read)
  return {
    sessionLog,
    runner: new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: plane,
      maxTokens: 8_000,
      maxTurns: 10,
      baselineToolIds: ["read"],
      ...(notesBudgetTokens === undefined ? {} : { contextPolicy: { notesBudgetTokens } }),
    }),
  }
}

describe("notes (plan task 9.1)", () => {
  it("are written by the model, kept by the kernel and shown in the task state", async () => {
    const { provider, offered, states } = scripted()
    const { runner: run, sessionLog } = runner(provider, 600)
    const answer = await collectText(run.run({ sessionId: "notes", goal: "count the errors" }))
    expect(answer).toBe("3")
    expect(offered[0]).toContain("note")
    expect(states[2]).toContain("[state] Tally: 3 qualifying lines in logs/a.log")

    const events = (await sessionLog.read("notes")).map(entry => entry.event)
    const toolResults = JSON.stringify(events.filter(event => event.kind === "tool_completed" || event.kind === "tool_failed"))
    expect(toolResults).not.toContain("unknown tool")
    const report = [...events].reverse().find(event => event.kind === "context_lifecycle_reported") as
      { notes_written?: number; notes_active?: number; edges?: Array<{ relation: string }> }
    expect(report.notes_written).toBe(1)
    expect(report.notes_active).toBe(1)
    expect(report.edges?.some(edge => edge.relation === "derived_from")).toBe(true)
  })

  it("are off without a budget", async () => {
    const { provider, offered } = scripted()
    const { runner: run } = runner(provider)
    await collectText(run.run({ sessionId: "no-notes", goal: "count the errors" }))
    expect(offered[0]).not.toContain("note")
  })

  it("reject a negative or fractional budget", () => {
    expect(() => contextPolicy({ notesBudgetTokens: -1 })).toThrow(/notesBudgetTokens/)
    expect(() => contextPolicy({ notesBudgetTokens: 1.5 })).toThrow(/notesBudgetTokens/)
    expect(normalizeContextPolicy(contextPolicy({ notesBudgetTokens: 600 })).notes_budget_tokens).toBe(600)
    expect(normalizeContextPolicy(contextPolicy()).notes_budget_tokens).toBeUndefined()
  })
})
