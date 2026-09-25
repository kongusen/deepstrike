import { count, metric } from "../core/metrics.mjs"
import { collectAsync } from "../core/runtime.mjs"

export const planesHarnessEvals = {
  id: "planes-harness-evals",
  description: "execution plane, AttemptLoop, and eval result/trace contracts",
  variants: ["deterministic"],
  surfaces: ["advanced", "harness", "evals", "root"],
  async run({ sdk }) {
    const echo = sdk.root.tool("echo", "Return a value", {
      type: "object", properties: { value: { type: "string" } }, required: ["value"], additionalProperties: false,
    }, args => String(args.value))
    const plane = new sdk.advanced.LocalExecutionPlane().register(echo)
    const toolEvents = await collectAsync(plane.executeAll([{ id: "echo-1", name: "echo", arguments: JSON.stringify({ value: "ok" }) }], {}))
    const toolResult = toolEvents.find(event => event.type === "tool_result")

    const attemptLoop = new sdk.harness.AttemptLoop({
      body: { async *run() { yield { type: "body_done", runStatus: "completed", result: "ok", turns: 1, totalTokens: 3 } } },
      judge: new sdk.harness.VerdictFnJudge(() => ({ passed: true, overallScore: 1, feedback: "ok", details: [] })),
      stop: { maxAttempts: 1 },
    })
    const attempt = await attemptLoop.run({ goal: "return ok", criteria: [{ text: "result is ok", machineCheckable: true }] })

    const evalRun = await sdk.evals.evaluate({
      run: async input => ({ output: `answer:${input}`, evidence: { route: { provider: "fixture" } } }),
    }, {
      dataset: { name: "fixture", cases: [{ id: "case-1", input: "one", expected: "answer:one" }] },
      evaluators: [{ name: "exact", evaluate: ({ testCase, output }) => output === testCase.expected ? 1 : 0 }],
      runId: "eval-benchmark",
      includeTrace: true,
    })

    if (toolResult?.content !== "ok" || toolResult?.isError) throw new Error("LocalExecutionPlane did not execute the registered tool")
    if (attempt.outcome !== "passed" || attempt.verdict?.passed !== true) throw new Error("AttemptLoop did not produce a passing verdict")
    if (!evalRun.completed || evalRun.results[0]?.scores.exact !== 1 || evalRun.traces?.[0]?.route?.provider !== "fixture") throw new Error("eval result/trace contract failed")

    return {
      metrics: {
        toolEvents: count(toolEvents.length),
        toolOutputChars: metric(String(toolResult.content).length, "chars"),
        attempts: count(attempt.attempts),
        evalCases: count(evalRun.results.length),
        evalScore: metric(evalRun.results[0].scores.exact, "score"),
      },
      evidence: {
        tool: toolEvents.map(event => ({ type: event.type, content: event.content })),
        attempt: { outcome: attempt.outcome, verdict: attempt.verdict },
        eval: evalRun,
      },
    }
  },
}
