/**
 * G3 structured output: the JSON-Schema-subset validator + the runWorkflow validate-retry path.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import type { WorkflowSpec } from "../src/index.js"
import {
  validateAgainstSchema,
  extractJsonValue,
  schemaInstruction,
} from "../src/runtime/output-schema.js"

describe("validateAgainstSchema (supported subset)", () => {
  const schema = {
    type: "object",
    required: ["verdict", "score"],
    properties: {
      verdict: { type: "string", enum: ["pass", "fail"] },
      score: { type: "integer" },
      notes: { type: "array", items: { type: "string" } },
    },
  }

  it("accepts a conforming object", () => {
    expect(validateAgainstSchema({ verdict: "pass", score: 3, notes: ["ok"] }, schema).ok).toBe(true)
  })

  it("flags a missing required property", () => {
    const r = validateAgainstSchema({ verdict: "pass" }, schema)
    expect(r.ok).toBe(false)
    expect(r.errors.join(" ")).toMatch(/score.*required/)
  })

  it("flags a wrong type and a non-integer number", () => {
    expect(validateAgainstSchema({ verdict: "pass", score: 1.5 }, schema).ok).toBe(false)
    expect(validateAgainstSchema("not-an-object", schema).ok).toBe(false)
  })

  it("flags an out-of-enum value and a bad array element", () => {
    expect(validateAgainstSchema({ verdict: "maybe", score: 1 }, schema).ok).toBe(false)
    expect(validateAgainstSchema({ verdict: "pass", score: 1, notes: [42] }, schema).ok).toBe(false)
  })
})

describe("extractJsonValue", () => {
  it("parses raw JSON, fenced JSON, and embedded JSON", () => {
    expect(extractJsonValue('{"a":1}')).toEqual({ a: 1 })
    expect(extractJsonValue('```json\n{"a":1}\n```')).toEqual({ a: 1 })
    expect(extractJsonValue('Here is the result: {"a":1}. Done.')).toEqual({ a: 1 })
  })
  it("returns undefined for non-JSON", () => {
    expect(extractJsonValue("no json here")).toBeUndefined()
  })
})

// ── runWorkflow validate-retry path ──────────────────────────────────────────────────────────────

const SCHEMA = { type: "object", required: ["verdict"], properties: { verdict: { type: "string" } } }

function node(agent_id: string) {
  return {
    agent_id,
    goal: "judge it",
    role: "verify",
    isolation: "read_only",
    context_inheritance: "none",
    trust: "trusted",
    output_schema: SCHEMA,
  }
}

/** Single-node workflow whose only node declares an output_schema; completes when node0 reports. */
function makeFakeKernel() {
  const reply = (actions: unknown[], observations: unknown[]) =>
    JSON.stringify({ version: 2, actions, observations })
  const spawn = {
    kind: "spawn_workflow",
    effect_id: "fake-workflow-spawn-1",
    nodes: [node("wf-node0")],
  }
  return {
    turn: () => 0,
    step(input: string): string {
      const { event } = JSON.parse(input) as { event: { kind: string; result?: { agent_id: string; result?: { termination?: string } } } }
      if (event.kind === "load_workflow") return reply([spawn], [])
      if (event.kind === "workflow_spawn_result") {
        return reply([], [{ kind: "workflow_batch_spawned", nodes: spawn.nodes }])
      }
      if (event.kind === "sub_agent_completed" && event.result?.agent_id === "wf-node0") {
        const failed = event.result.result?.termination === "error"
        return reply([], [{
          kind: "workflow_completed",
          completed: failed ? [] : ["wf-node0"],
          failed: failed ? ["wf-node0"] : [],
        }])
      }
      return reply([], [])
    },
  }
}

function wire(runner: RuntimeRunner, kernel: unknown) {
  ;(runner as never as { activeKernel: unknown }).activeKernel = kernel
  ;(runner as never as { currentSessionId: string }).currentSessionId = "wf-g3"
  ;(runner as never as { pendingObservations: unknown[] }).pendingObservations = []
}

const spec: WorkflowSpec = { nodes: [{ task: "judge it", role: "verify", outputSchema: SCHEMA }] }

describe("runWorkflow enforces output_schema", () => {
  it("instructs the agent and accepts conforming output on the first attempt", async () => {
    const goals: string[] = []
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string }; spec: { goal: string } }) {
        goals.push(ctx.spec.goal)
        return {
          agentId: ctx.manifest.agent_id,
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: '{"verdict":"pass"}', toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      },
    }
    const runner = new RuntimeRunner({ sessionLog: new InMemorySessionLog(), maxTokens: 8000, subAgentOrchestrator: orchestrator as never } as never)
    wire(runner, makeFakeKernel())
    const outcome = await runner.runWorkflow(spec)
    expect(outcome.completed).toEqual(["wf-node0"])
    expect(goals).toHaveLength(1)
    expect(goals[0]).toContain(schemaInstruction(SCHEMA))
  })

  it("re-runs once with the validation errors when the first output is invalid, then accepts the fix", async () => {
    let calls = 0
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string }; spec: { goal: string } }) {
        calls += 1
        const content = calls === 1 ? "I think it passes." : '{"verdict":"pass"}'
        // The retry prompt must carry the prior failure.
        if (calls === 2) expect(ctx.spec.goal).toMatch(/did NOT conform/)
        return {
          agentId: ctx.manifest.agent_id,
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content, toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      },
    }
    const runner = new RuntimeRunner({ sessionLog: new InMemorySessionLog(), maxTokens: 8000, subAgentOrchestrator: orchestrator as never } as never)
    wire(runner, makeFakeKernel())
    const outcome = await runner.runWorkflow(spec)
    expect(calls).toBe(2)
    expect(outcome.completed).toEqual(["wf-node0"])
  })

  it("fails the node when output never conforms (after the retry)", async () => {
    let calls = 0
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string } }) {
        calls += 1
        return {
          agentId: ctx.manifest.agent_id,
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: "never valid json", toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      },
    }
    const runner = new RuntimeRunner({ sessionLog: new InMemorySessionLog(), maxTokens: 8000, subAgentOrchestrator: orchestrator as never } as never)
    wire(runner, makeFakeKernel())
    const outcome = await runner.runWorkflow(spec)
    expect(calls).toBe(2) // tried, retried, still invalid
    expect(outcome.failed).toEqual(["wf-node0"])
  })

  it("uses the SDK-configured validation attempt bound", async () => {
    let calls = 0
    const orchestrator = {
      async run(ctx: { manifest: { agent_id: string } }) {
        calls += 1
        return {
          agentId: ctx.manifest.agent_id,
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: "never valid json", toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      },
    }
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: orchestrator as never,
      workflowSchemaValidationAttempts: 3,
    } as never)
    wire(runner, makeFakeKernel())
    const outcome = await runner.runWorkflow(spec)
    expect(calls).toBe(3)
    expect(outcome.failed).toEqual(["wf-node0"])
  })

  it("rejects an unsafe validation attempt bound", () => {
    expect(() => new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      workflowSchemaValidationAttempts: 0,
    } as never)).toThrow(/between 1 and 16/)
  })
})
