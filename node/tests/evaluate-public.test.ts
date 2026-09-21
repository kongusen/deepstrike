import { evaluate } from "../src/evals/public.js"

test("SPC-028-43 public evaluate runs Dataset cases through Evaluators", async () => {
  const run = await evaluate({ run: async input => ({ output: input.toUpperCase() }) }, {
    dataset: { name: "smoke", cases: [{ id: "one", input: "hello" }] },
    evaluators: [{ name: "contains-uppercase", evaluate: ({ output }) => output === "HELLO" ? 1 : 0 }],
    runId: "eval-1",
  })
  expect(run).toEqual({ runId: "eval-1", completed: true, results: [{ caseId: "one", output: "HELLO", scores: { "contains-uppercase": 1 } }] })
})

test("SPC-028-44 keeps execution evidence in optional eval traces", async () => {
  const run = await evaluate({ run: async input => ({
    output: input.toUpperCase(),
    route: { provider: "test" },
    usage: { inputTokens: 2, outputTokens: 1 },
    artifacts: ["artifact-1"],
  }) }, {
    dataset: { cases: [{ id: "one", input: "hello", metadata: { tenant: "demo" } }] },
    evaluators: [],
    includeTrace: true,
    runId: "eval-trace-1",
  })
  expect(run.results).toEqual([{ caseId: "one", output: "HELLO", scores: {} }])
  expect(run.traces).toEqual([{
    caseId: "one",
    executedInput: "hello",
    contextBinding: { tenant: "demo" },
    route: { provider: "test" },
    measurement: { inputTokens: 2, outputTokens: 1 },
    artifactSet: ["artifact-1"],
  }])
})
