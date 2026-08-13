import {
  workflowNodeSpecToKernel,
  subAgentResultToKernel,
} from "../src/types/agent.js"
import type { SubAgentResult } from "../src/types/agent.js"
import {
  loopInstruction,
  classifyInstruction,
  judgeGoal,
  extractClassifyBranch,
  extractJudgeWinner,
} from "../src/runtime/workflow-control-flow.js"

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
