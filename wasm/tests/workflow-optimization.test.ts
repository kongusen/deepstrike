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
import { dependencyOutputsNote } from "../src/runtime/workflow-control-flow.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"
import type { RegisteredTool } from "../src/tools/index.js"

type Obs = Record<string, unknown>
const reply = (obs: Obs[]) => JSON.stringify({ version: 1, actions: [], observations: obs })

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
  ;(runner as unknown as { activeKernel: unknown }).activeKernel = fakeKernel
  ;(runner as unknown as { currentSessionId: string }).currentSessionId = sessionId
  ;(runner as unknown as { pendingObservations: unknown[] }).pendingObservations = []
  return { runner, sessionLog, plane }
}

const idleProvider: LLMProvider = {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "done" }
  },
}

describe("W-1: resume replays control flow over the ABI wire", () => {
  it("resumeWorkflow lowers recorded resumed_results (classify branch) and re-seeds dependent goals from persisted outputs", async () => {
    const seen: Array<Record<string, unknown>> = []
    // Scripted kernel: honors the resumed classify choice — only branch "a" spawns (branch "b"
    // stays pruned), and the dependent carries its W-N2 data edge to the completed classifier.
    const fake = {
      turn: () => 0,
      step(input: string): string {
        const { event } = JSON.parse(input) as { event: Record<string, unknown> }
        seen.push(event)
        if (event.kind === "load_workflow") {
          return reply([{
            kind: "workflow_batch_spawned",
            nodes: [spawn({ agent_id: "wf-node1", goal: "on a", input_agent_ids: ["wf-node0"] })],
          }])
        }
        if (event.kind === "sub_agent_completed") {
          return reply([{ kind: "workflow_completed", completed: ["wf-node0", "wf-node1"], failed: [] }])
        }
        return reply([])
      },
    }
    const contexts: Array<{ goal: string; toolAccess?: string }> = []
    const orchestrator = {
      async run(ctx: { spec: { goal: string }; manifest: { agent_id: string }; toolAccess?: string }) {
        contexts.push({ goal: ctx.spec.goal, toolAccess: ctx.toolAccess })
        return {
          agentId: ctx.manifest.agent_id,
          result: { termination: "completed", finalMessage: { role: "assistant", content: "branch work done", toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 },
        }
      },
    }
    const { runner, sessionLog } = createWorkflowRunner(
      idleProvider, fake, "wfresume", [], { subAgentOrchestrator: orchestrator },
    )
    // Pre-crash history (W-1): the classifier completed, chose branch "a", output persisted.
    await sessionLog.append("wfresume", {
      kind: "workflow_node_completed", turn: 1, agent_id: "wf-node0",
      termination: "completed", classify_branch: "a", output: "routing notes: choose a",
    })

    const spec: WorkflowSpec = {
      nodes: [
        { task: "route", role: "plan", classify: { branches: [{ label: "a", nodes: [1] }, { label: "b", nodes: [2] }] } },
        { task: "on a", role: "implement", dependsOn: [0] },
        { task: "on b", role: "implement", dependsOn: [0] },
      ],
    }
    const outcome = await runner.resumeWorkflow(spec, { sessionId: "wfresume" })

    // The signal-carrying record went over the wire — the kernel can re-prune the rejected branch.
    const load = seen.find(e => e.kind === "load_workflow") as Record<string, unknown>
    expect(load.resumed_results).toEqual([{ agent_id: "wf-node0", classify_branch: "a" }])
    // W-1 + W-N2: the post-resume dependent still sees its (pre-crash) dependency's output.
    expect(contexts[0].goal).toContain("[dependency wf-node0 output]\nrouting notes: choose a")
    // W-N1: a trusted workflow node asks for plane inheritance.
    expect(contexts[0].toolAccess).toBe("inherit")
    expect(outcome.completed).toEqual(["wf-node0", "wf-node1"])
  })

  it("subAgentResultToKernel strips the SDK-internal paceDecision but keeps loop_continue", () => {
    const wire = subAgentResultToKernel({
      agentId: "wf-node0-i1",
      result: {
        termination: "completed", turnsUsed: 1, totalTokensUsed: 1,
        loopContinue: false, paceDecision: { action: "stop", reason: "task complete" },
      },
    })
    expect(JSON.stringify(wire)).not.toContain("pace")
    expect((wire.result as Record<string, unknown>).loop_continue).toBe(false)
  })
})

describe("W-N2 / W-N7: spawn descriptors carry data edges and per-node caps", () => {
  it("workflowNodeSpecToKernel emits max_turns/max_wall_ms and workflowNodeToSpec maps them back", () => {
    const kernelJson = workflowNodeSpecToKernel({
      task: "expensive", role: "implement", tokenBudget: 5000, maxTurns: 4, maxWallMs: 30_000,
    })
    expect(kernelJson.max_turns).toBe(4)
    expect(kernelJson.max_wall_ms).toBe(30_000)

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

  it("dependencyOutputsNote formats, clips, and skips empty outputs", () => {
    const outputs = new Map([
      ["wf-node0", "alpha findings"],
      ["wf-node1", "x".repeat(9000)],
    ])
    const note = dependencyOutputsNote(["wf-node0", "wf-node1", "wf-node-missing"], outputs, 100)
    expect(note).toContain("[dependency wf-node0 output]\nalpha findings")
    expect(note).toContain("…[truncated]")
    expect(note).not.toContain("wf-node-missing")
    expect(dependencyOutputsNote([], outputs)).toBe("")
    expect(dependencyOutputsNote(undefined, outputs)).toBe("")
  })
})

describe("W-N1: workflow nodes get tools (trusted inherit; quarantined stay deny-all)", () => {
  function nodeProvider(): LLMProvider {
    let call = 0
    return {
      async complete(): Promise<Message> {
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
          return reply([{ kind: "workflow_completed", completed: ["wf-node0"], failed: [] }])
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
    expect(outcome.completed).toEqual(["wf-node0"])
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
    expect(outcome.completed).toEqual(["wf-node0"])
    expect(pings).toBe(0) // untrusted-content reader: no tool reaches the host
  })
})

describe("DW-3/W-N6: loop nodes pace through the kernel trap on ONE stable session", () => {
  /** Per ITERATION the child makes two calls: propose a pace verb, then file the report turn. */
  function pacingLoopProvider(verbs: string[]): LLMProvider {
    let call = 0
    return {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        call += 1
        const iteration = Math.ceil(call / 2) - 1
        if (call % 2 === 1) {
          yield {
            type: "tool_call", id: `pace-${call}`, name: "pace",
            arguments: { next: verbs[Math.min(iteration, verbs.length - 1)], reason: `iter ${iteration}` },
          }
          return
        }
        yield { type: "text_delta", delta: `iteration ${iteration} report` }
      },
    }
  }

  /** Scripted parent kernel mirroring NodeKind::Loop: iteration k spawns as `wf-node0-i{k}`
   *  with `loop_max_iters`; an explicit loop_continue=false ends the loop, an explicit true
   *  re-arms, and NO opinion (v1 fallback) re-arms until the cap. */
  function loopParentKernel(maxIters: number) {
    let iter = 0
    const loopSpawn = (k: number) =>
      spawn({ agent_id: `wf-node0-i${k}`, goal: "polish until done", loop_max_iters: maxIters })
    return {
      turn: () => 0,
      step(input: string): string {
        const { event } = JSON.parse(input) as {
          event: { kind: string; result?: { result?: { loop_continue?: boolean } } }
        }
        if (event.kind === "load_workflow") {
          return reply([{ kind: "workflow_batch_spawned", nodes: [loopSpawn(0)] }])
        }
        if (event.kind === "sub_agent_completed") {
          const cont = event.result?.result?.loop_continue
          if (cont !== false && iter + 1 < maxIters) {
            iter += 1
            return reply([{ kind: "workflow_batch_spawned", nodes: [loopSpawn(iter)] }])
          }
          return reply([{ kind: "workflow_completed", completed: ["wf-node0"], failed: [] }])
        }
        return reply([])
      },
    }
  }

  it("pace continue→stop drives the iterations; the transcript accumulates under one session id", async () => {
    const { runner, sessionLog } = createWorkflowRunner(
      pacingLoopProvider(["continue", "stop"]), loopParentKernel(5), "wfloop",
    )
    const outcome = await runner.runWorkflow(
      { nodes: [{ task: "polish until done", role: "implement", loop: { maxIters: 5 } }] },
    )
    expect(outcome.completed).toEqual(["wf-node0"])
    // The pace verb ended the loop at 2 iterations, well before maxIters=5.
    const loopSession = await sessionLog.read("wfloop-wf-node0")
    const starts = loopSession.filter(e => e.event.kind === "run_started")
    expect(starts.length).toBe(2) // W-N6: BOTH iterations ran under the ONE stable session id
    // No per-iteration session fragments.
    expect(await sessionLog.read("wfloop-wf-node0-i0")).toEqual([])
    expect(await sessionLog.read("wfloop-wf-node0-i1")).toEqual([])
  })

  it("an iteration that never paces completes the loop (silence = done, not run-to-cap)", async () => {
    const silent: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "all done in one pass" }
      },
    }
    const { runner, sessionLog } = createWorkflowRunner(silent, loopParentKernel(4), "wfsilent")
    const outcome = await runner.runWorkflow(
      { nodes: [{ task: "one-shot polish", role: "implement", loop: { maxIters: 4 } }] },
    )
    expect(outcome.completed).toEqual(["wf-node0"])
    // default_action=stop: exactly ONE iteration ran (the kernel's pace fallback said stop).
    const starts = (await sessionLog.read("wfsilent-wf-node0")).filter(e => e.event.kind === "run_started")
    expect(starts.length).toBe(1)
  })
})
