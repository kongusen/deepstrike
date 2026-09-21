/** Public evaluation language. Runtime evidence remains available through the runtime subpath. */
export { judge, buildEvalMessages, parseVerdict, verdictOutputSchema } from "../runtime/eval.js"
export type { Criterion, Verdict, VerdictDetail, JudgeArgs } from "../runtime/eval.js"

export interface DatasetCase { id: string; input: string; expected?: unknown; metadata?: Record<string, unknown> }
export interface Dataset { name?: string; cases: DatasetCase[] }
export interface Evaluator { name: string; evaluate(input: { testCase: DatasetCase; output: string }): Promise<number> | number }
export interface EvalResult { caseId: string; output: string; scores: Record<string, number> }
export interface EvalRun { runId: string; results: EvalResult[]; completed: boolean }

export async function evaluate(
  agent: { run(input: string): Promise<{ output: string }> },
  options: { dataset: Dataset; evaluators: Evaluator[]; runId?: string },
): Promise<EvalRun> {
  const results: EvalResult[] = []
  for (const testCase of options.dataset.cases) {
    const output = await agent.run(testCase.input)
    const scores: Record<string, number> = {}
    for (const evaluator of options.evaluators) scores[evaluator.name] = await evaluator.evaluate({ testCase, output: output.output })
    results.push({ caseId: testCase.id, output: output.output, scores })
  }
  return { runId: options.runId ?? crypto.randomUUID(), results, completed: true }
}
