/**
 * Dynamic-workflow optimization batch (wasm port of node's workflow-optimization.test.ts):
 * W-N1 tool exposure, W-N2 data edges, W-N7 per-node caps, W-1 resume signal replay, and the
 * DW-3/W-N6 pace-vocabulary loop unification. WASM has no native kernel in tests, so scripted
 * fake kernels reproduce the parent-side observation sequences the Rust kernel emits (the same
 * convention as bootstrap-workflow / workflow-preempt); the CHILD runs against the shared
 * `__mocks__/kernel.ts`, which mirrors the kernel's pacing trap (`run_spec.loop_round` → trapped
 * `pace` call → `pace_decision` on done).
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/index.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import {
  workflowNodeSpecToKernel, workflowNodeToSpec, subAgentResultToKernel,
} from "../src/runtime/types/agent.js"
import type { WorkflowSpec } from "../src/index.js"
import type { LLMProvider, ModelMessage, StreamEvent } from "../src/types.js"
import type { RegisteredTool } from "../src/tools/index.js"
import { wrapScriptedKernel } from "./helpers/scripted-canonical-runtime.js"

type Obs = Record<string, unknown>
let effectSequence = 1
const reply = (obs: Obs[]) => {
  const batch = obs.find(o => o.kind === "workflow_batch_spawned")
  const completed = obs.some(o => o.kind === "workflow_completed")
  const actions = batch
    ? [{ kind: "spawn_workflow", effect_id: `spawn-${effectSequence++}`, nodes: batch.nodes, ...(batch.budget ? { budget: batch.budget } : {}) }]
    : completed
      ? [{ kind: "call_provider", effect_id: `provider-${effectSequence++}`, context: {}, tools: [] }]
      : []
  return JSON.stringify({
    version: 2,
    actions,
    observations: obs.filter(o => o.kind !== "workflow_batch_spawned"),
    faults: [],
  })
}

/** A workflow_batch_spawned node descriptor (kernel wire shape). */
const spawn = (over: Record<string, unknown> = {}) => ({
  agent_id: "wf-node0", goal: "g", role: "implement", isolation: "shared",
  context_inheritance: "none", model_hint: null, trust: "trusted", ...over,
})

/** Build a runner whose ACTIVE kernel is a scripted fake (the wasm test convention for the
 *  parent side); children spawned by the real orchestrator run on the shared mock kernel. */
function createWorkflowRunner(
  provider: LLMProvider,
  fakeKernel: unknown,
  sessionId: string,
  tools: RegisteredTool[] = [],
  extraOpts: Record<string, unknown> = {},
) {
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  for (const t of tools) plane.register(t)
  const runner = new RuntimeRunner({
    provider,
    executionPlane: plane,
    sessionLog,
    maxTokens: 8000,
    ...extraOpts,
  } as never)
  ;(runner as unknown as { activeKernel: unknown }).activeKernel = wrapScriptedKernel(fakeKernel as { step(inputJson: string): string })
  ;(runner as unknown as { currentSessionId: string }).currentSessionId = sessionId
  ;(runner as unknown as { pendingObservations: unknown[] }).pendingObservations = []
  return { runner, sessionLog, plane }
}

const idleProvider: LLMProvider = {
  async complete(): Promise<ModelMessage> {
    return { role: "assistant", content: "done", toolCalls: [] }
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "done" }
  },
}

describe("workflow result wire projection", () => {
  it("subAgentResultToKernel strips the SDK-internal paceDecision", () => {
    const wire = subAgentResultToKernel({
      agentId: "wf-node0",
      result: {
        termination: "completed", turnsUsed: 1, totalTokensUsed: 1,
        paceDecision: { action: "stop", reason: "task complete" },
      },
    })
    expect(JSON.stringify(wire)).not.toContain("pace")
  })
})

describe("W-N2 / W-N7: spawn descriptors carry data edges and per-node caps", () => {
  it("workflowNodeSpecToKernel emits max_turns/max_wall_ms and workflowNodeToSpec maps them back", () => {
    const kernelJson = workflowNodeSpecToKernel({
      task: "expensive", role: "implement", tokenBudget: 5000, maxTurns: 4, maxWallMs: 30_000,
    })
    expect(kernelJson.max_turns).toBe(4)
    expect(kernelJson.max_wall_ms).toBe(30_000)
    expect(kernelJson.dep_policy).toBe("all_success")
    expect(workflowNodeSpecToKernel({ task: "x", role: "plan", depPolicy: "accept_partial" }).dep_policy)
      .toBe("accept_partial")

    const spec = workflowNodeToSpec(
      {
        agent_id: "wf-node0", goal: "g", role: "implement", isolation: "shared",
        context_inheritance: "none", token_budget: 5000, max_turns: 4, max_wall_ms: 30_000,
      },
      "parent",
    )
    expect(spec.maxTurns).toBe(4)
    expect(spec.maxWallMs).toBe(30_000)
    expect(spec.tokenBudget).toBe(5000)
  })
})

describe("W-N1: workflow nodes get tools (trusted inherit; quarantined stay deny-all)", () => {
  function nodeProvider(): LLMProvider {
    let call = 0
    return {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        call += 1
        if (call === 1) {
          yield { type: "tool_call", id: `t-${call}`, name: "ping", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "node done" }
      },
    }
  }

  /** Scripted parent kernel: one plain node with the given trust; completion ends the workflow. */
  function trustParentKernel(trust: string) {
    return {
      turn: () => 0,
      step(input: string): string {
        const { event } = JSON.parse(input) as { event: { kind: string } }
        if (event.kind === "load_workflow") {
          return reply([{ kind: "workflow_batch_spawned", nodes: [spawn({ trust })] }])
        }
        if (event.kind === "sub_agent_completed") {
          return reply([{ kind: "workflow_completed", node_outcomes: [{ node_id: "wf-node0", status: "completed" }] }])
        }
        return reply([])
      },
    }
  }

  it("a trusted workflow node can call the parent's registered tools", async () => {
    let pings = 0
    const ping = tool("ping", "ping the host", { type: "object", properties: {} }, async () => {
      pings += 1
      return "pong"
    })
    const { runner } = createWorkflowRunner(nodeProvider(), trustParentKernel("trusted"), "wftools", [ping])
    const outcome = await runner.runWorkflow({ nodes: [{ task: "use the ping tool once, then stop", role: "implement" }] })
    expect(outcome.nodeOutcomes.map(node => node.nodeId)).toEqual(["wf-node0"])
    expect(pings).toBe(1) // pre-W-N1 this was 0: the missing grant list ran every node TOOL-LESS
  })

  it("a quarantined workflow node stays deny-all filtered", async () => {
    let pings = 0
    const ping = tool("ping", "ping the host", { type: "object", properties: {} }, async () => {
      pings += 1
      return "pong"
    })
    const { runner } = createWorkflowRunner(nodeProvider(), trustParentKernel("quarantined"), "wfquar", [ping])
    const outcome = await runner.runWorkflow({
      nodes: [{ task: "try the ping tool", role: "explore", isolation: "read_only", trust: "quarantined" }],
    })
    expect(outcome.nodeOutcomes.map(node => node.nodeId)).toEqual(["wf-node0"])
    expect(pings).toBe(0) // untrusted-content reader: no tool reaches the host
  })
})
