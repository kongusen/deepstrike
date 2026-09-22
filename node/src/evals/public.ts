/** Public evaluation language. Runtime evidence remains available through the runtime subpath. */
export { judge, buildEvalMessages, parseVerdict, verdictOutputSchema } from "../runtime/eval.js"
export type { Criterion, Verdict, VerdictDetail, JudgeArgs } from "../runtime/eval.js"

export interface DatasetCase { id: string; input: string; expected?: unknown; metadata?: Record<string, unknown> }
export interface Dataset { name?: string; cases: DatasetCase[] }
export interface Evaluator { name: string; evaluate(input: { testCase: DatasetCase; output: string }): Promise<number> | number }
export interface EvalResult { caseId: string; output: string; scores: Record<string, number> }
export interface ExecutionEvidence {
  contextBinding?: unknown
  route?: unknown
  measurement?: unknown
  artifactSet?: unknown
}
/** Optional execution evidence kept separate from the stable score/result contract. */
export interface EvalTrace {
  caseId: string
  executedInput: string
  contextBinding?: unknown
  route?: unknown
  measurement?: unknown
  artifactSet?: unknown
}
export interface EvalRun { runId: string; results: EvalResult[]; completed: boolean; traces?: EvalTrace[] }

export async function evaluate(
  agent: { run(input: string): Promise<{ output: string; evidence?: ExecutionEvidence }> },
  options: { dataset: Dataset; evaluators: Evaluator[]; runId?: string; includeTrace?: boolean },
): Promise<EvalRun> {
  const results: EvalResult[] = []
  const traces: EvalTrace[] = []
  for (const testCase of options.dataset.cases) {
    const output = await agent.run(testCase.input)
    const scores: Record<string, number> = {}
    for (const evaluator of options.evaluators) scores[evaluator.name] = await evaluator.evaluate({ testCase, output: output.output })
    results.push({ caseId: testCase.id, output: output.output, scores })
    if (options.includeTrace) {
      traces.push({
        caseId: testCase.id,
        executedInput: testCase.input,
        ...(output.evidence?.contextBinding !== undefined ? { contextBinding: output.evidence.contextBinding } : {}),
        ...(output.evidence?.route !== undefined ? { route: output.evidence.route } : {}),
        ...(output.evidence?.measurement !== undefined ? { measurement: output.evidence.measurement } : {}),
        ...(output.evidence?.artifactSet !== undefined ? { artifactSet: output.evidence.artifactSet } : {}),
      })
    }
  }
  return { runId: options.runId ?? crypto.randomUUID(), results, completed: true, ...(options.includeTrace ? { traces } : {}) }
}
