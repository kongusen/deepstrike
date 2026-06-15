import { getKernel } from "../src/kernel.js"
import {
  workflowSpecToKernel,
  workflowNodeSpecToKernel,
  subAgentResultToKernel,
} from "../src/types/agent.js"
import type { WorkflowSpec, WorkflowSpawnInfo, SubAgentResult } from "../src/types/agent.js"
import {
  loopInstruction,
  classifyInstruction,
  judgeGoal,
  extractLoopContinue,
  extractClassifyBranch,
  extractJudgeWinner,
} from "../src/runtime/workflow-control-flow.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return JSON.parse(rt.step(JSON.stringify({ version: 1, event }))) as {
    actions: Array<Record<string, unknown>>
    observations: Array<{ kind: string; nodes?: WorkflowSpawnInfo[]; completed?: string[]; failed?: string[] }>
  }
}

const batchOf = (obs: ReturnType<typeof step>["observations"]): WorkflowSpawnInfo[] =>
  (obs.find(o => o.kind === "workflow_batch_spawned")?.nodes ?? [])
const doneOf = (obs: ReturnType<typeof step>["observations"]) =>
  obs.find(o => o.kind === "workflow_completed")

/** Feed a sub_agent_completed carrying optional control-flow signals. */
function complete(
  rt: { step(json: string): string },
  agentId: string,
  signals: { loopContinue?: boolean; classifyBranch?: string; tournamentWinner?: string } = {},
) {
  const result: SubAgentResult = {
    agentId,
    result: {
      termination: "completed",
      finalMessage: { role: "assistant", content: "ok", toolCalls: [] },
      turnsUsed: 1,
      totalTokensUsed: 1,
      ...signals,
    },
  }
  return step(rt, { kind: "sub_agent_completed", result: subAgentResultToKernel(result) })
}

describe("workflowNodeSpecToKernel: control-flow kinds", () => {
  it("maps loop / classify / tournament / reduce to serde-tagged NodeKind JSON", () => {
    expect(workflowNodeSpecToKernel({ task: "refine", role: "implement", loop: { maxIters: 3 } }).kind).toEqual({
      type: "loop",
      max_iters: 3,
    })
    expect(
      workflowNodeSpecToKernel({
        task: "route",
        role: "plan",
        classify: { branches: [{ label: "bug", nodes: [1] }, { label: "feature", nodes: [2] }] },
      }).kind,
    ).toEqual({ type: "classify", branches: [{ label: "bug", nodes: [1] }, { label: "feature", nodes: [2] }] })
    expect(
      workflowNodeSpecToKernel({
        task: "pick best",
        role: "plan",
        tournament: { entrants: ["a", { goal: "b", criteria: ["x"] }] },
      }).kind,
    ).toEqual({ type: "tournament", entrants: [{ goal: "a", criteria: [] }, { goal: "b", criteria: ["x"] }] })
    expect(workflowNodeSpecToKernel({ task: "merge", role: "custom", reducer: "concat" }).kind).toEqual({
      type: "reduce",
      reducer: "concat",
    })
  })

  it("a plain spawn node omits kind entirely (byte-identical to before)", () => {
    expect("kind" in workflowNodeSpecToKernel({ task: "do", role: "implement" })).toBe(false)
  })

  it("maps tokenBudget → token_budget (M4/G5), omitted when unset", () => {
    expect(workflowNodeSpecToKernel({ task: "x", role: "plan", tokenBudget: 10000 }).token_budget).toBe(10000)
    expect("token_budget" in workflowNodeSpecToKernel({ task: "x", role: "plan" })).toBe(false)
  })

  it("rejects a node declaring more than one control-flow kind", () => {
    expect(() =>
      workflowNodeSpecToKernel({ task: "x", role: "plan", loop: { maxIters: 2 }, reducer: "concat" }),
    ).toThrow(/at most one/)
  })
})

describe("subAgentResultToKernel: control-flow signals", () => {
  const base: SubAgentResult = {
    agentId: "wf-node0",
    result: { termination: "completed", turnsUsed: 1, totalTokensUsed: 1 },
  }

  it("emits each signal only when set (additive, omitted otherwise)", () => {
    const plain = subAgentResultToKernel(base).result as Record<string, unknown>
    expect("loop_continue" in plain).toBe(false)
    expect("classify_branch" in plain).toBe(false)
    expect("tournament_winner" in plain).toBe(false)

    const loop = subAgentResultToKernel({ ...base, result: { ...base.result, loopContinue: false } })
      .result as Record<string, unknown>
    expect(loop.loop_continue).toBe(false)

    const clf = subAgentResultToKernel({ ...base, result: { ...base.result, classifyBranch: "bug" } })
      .result as Record<string, unknown>
    expect(clf.classify_branch).toBe("bug")

    const trn = subAgentResultToKernel({ ...base, result: { ...base.result, tournamentWinner: "wf-node2" } })
      .result as Record<string, unknown>
    expect(trn.tournament_winner).toBe("wf-node2")
  })
})

describe("control-flow extractors", () => {
  it("extractLoopContinue reads loop_continue / done; undefined when absent", () => {
    expect(extractLoopContinue('{"loop_continue": false}')).toBe(false)
    expect(extractLoopContinue('{"loop_continue": true}')).toBe(true)
    expect(extractLoopContinue('done now: {"done": true}')).toBe(false)
    expect(extractLoopContinue("no json here")).toBeUndefined()
  })

  it("extractClassifyBranch prefers {branch}, falls back to a bare valid label", () => {
    expect(extractClassifyBranch('{"branch": "bug"}', ["bug", "feature"])).toBe("bug")
    expect(extractClassifyBranch("feature", ["bug", "feature"])).toBe("feature")
    expect(extractClassifyBranch("garbage", ["bug", "feature"])).toBeUndefined()
  })

  it("extractJudgeWinner returns left/right and defaults to left on ambiguity", () => {
    expect(extractJudgeWinner('{"winner": "right"}')).toBe("right")
    expect(extractJudgeWinner('{"winner": "left"}')).toBe("left")
    expect(extractJudgeWinner("the right candidate wins")).toBe("right")
    expect(extractJudgeWinner("totally unparseable")).toBe("left")
  })

  it("instruction builders mention the cap / labels / candidates", () => {
    expect(loopInstruction(4)).toContain("4")
    expect(classifyInstruction(["bug", "feature"])).toContain('"bug"')
    expect(judgeGoal("which is best", "LEFTOUT", "RIGHTOUT")).toContain("LEFTOUT")
  })
})

describe("LoadWorkflow ABI drives control-flow kinds end-to-end", () => {
  const newRt = () => new (getKernel().KernelRuntime)({ maxTokens: 128_000 }) as { step(json: string): string }
  const start = (rt: { step(json: string): string }, spec: WorkflowSpec) => {
    step(rt, { kind: "start_run", task: { goal: "parent", criteria: [] } })
    return step(rt, { kind: "load_workflow", spec: workflowSpecToKernel(spec), parent_session_id: "sess" })
  }

  it("loop: descriptor carries loop_max_iters; loop_continue=false stops early and promotes the dependent", () => {
    const rt = newRt()
    const spec: WorkflowSpec = {
      nodes: [
        { task: "refine", role: "implement", loop: { maxIters: 5 } },
        { task: "ship", role: "implement", dependsOn: [0] },
      ],
    }
    const loaded = start(rt, spec)
    const b1 = batchOf(loaded.observations)
    expect(b1).toHaveLength(1)
    expect(b1[0].agent_id).toBe("wf-node0-i0")
    expect(b1[0].loop_max_iters).toBe(5)

    // iteration 0 signals "done" early → loop stops before max_iters → dependent unblocks.
    const after = complete(rt, "wf-node0-i0", { loopContinue: false })
    const b2 = batchOf(after.observations)
    expect(b2.map(n => n.agent_id)).toEqual(["wf-node1"])

    const fin = complete(rt, "wf-node1")
    // The loop node's completed entry is its base node id (`wf-node0`), not the iteration id.
    expect(doneOf(fin.observations)?.completed).toEqual(expect.arrayContaining(["wf-node0", "wf-node1"]))
  })

  it("classify: descriptor carries classify_labels; the chosen branch runs and the rest are pruned", () => {
    const rt = newRt()
    const spec: WorkflowSpec = {
      nodes: [
        { task: "route", role: "plan", classify: { branches: [{ label: "a", nodes: [1] }, { label: "b", nodes: [2] }] } },
        { task: "branch-a", role: "implement", dependsOn: [0] },
        { task: "branch-b", role: "implement", dependsOn: [0] },
      ],
    }
    const loaded = start(rt, spec)
    const b1 = batchOf(loaded.observations)
    expect(b1[0].agent_id).toBe("wf-node0")
    expect(b1[0].classify_labels).toEqual(["a", "b"])

    // classifier picks "a" → only branch-a (node 1) spawns; branch-b (node 2) is pruned/failed.
    const after = complete(rt, "wf-node0", { classifyBranch: "a" })
    expect(batchOf(after.observations).map(n => n.agent_id)).toEqual(["wf-node1"])

    const fin = complete(rt, "wf-node1")
    const done = doneOf(fin.observations)
    expect(done?.completed).toEqual(expect.arrayContaining(["wf-node0", "wf-node1"]))
    expect(done?.failed).toEqual(["wf-node2"])
  })

  it("tournament: entrants carry no judge_match; judges do; the winner promotes the dependent", () => {
    const rt = newRt()
    const spec: WorkflowSpec = {
      nodes: [
        { task: "pick the best", role: "plan", tournament: { entrants: ["x", "y"] } },
        { task: "use winner", role: "implement", dependsOn: [0] },
      ],
    }
    const loaded = start(rt, spec)
    // The controller expands into 2 entrant children (no judge_match on entrants).
    const entrants = batchOf(loaded.observations)
    expect(entrants).toHaveLength(2)
    expect(entrants.every(n => n.judge_match == null)).toBe(true)
    const entrantIds = entrants.map(n => n.agent_id)

    // Finish both entrants → a judge with a judge_match over the two candidates is emitted.
    let judges: WorkflowSpawnInfo[] = []
    for (const id of entrantIds) judges = batchOf(complete(rt, id).observations)
    expect(judges).toHaveLength(1)
    const jm = judges[0].judge_match
    expect(jm).toBeDefined()
    expect([jm!.left, jm!.right].sort()).toEqual([...entrantIds].sort())

    // The judge reports a winner → bracket resolves → the controller completes → dependent unblocks.
    const afterJudge = complete(rt, judges[0].agent_id, { tournamentWinner: jm!.left })
    expect(batchOf(afterJudge.observations).map(n => n.agent_id)).toEqual(["wf-node1"])

    const fin = complete(rt, "wf-node1")
    expect(doneOf(fin.observations)?.completed).toEqual(expect.arrayContaining(["wf-node0", "wf-node1"]))
  })
})
