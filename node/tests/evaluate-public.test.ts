import { evaluate } from "../src/evals/public.js"

test("SPC-028-43 public evaluate runs Dataset cases through Evaluators", async () => {
  const run = await evaluate({ run: async input => ({ output: input.toUpperCase() }) }, {
    dataset: { name: "smoke", cases: [{ id: "one", input: "hello" }] },
    evaluators: [{ name: "contains-uppercase", evaluate: ({ output }) => output === "HELLO" ? 1 : 0 }],
    runId: "eval-1",
  })
  expect(run).toEqual({ runId: "eval-1", completed: true, results: [{ caseId: "one", output: "HELLO", scores: { "contains-uppercase": 1 } }] })
})
