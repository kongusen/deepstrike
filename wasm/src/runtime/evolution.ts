import { getKernel } from "./kernel.js"

export type ArtifactKind = "runtime" | "skill" | "prompt" | "policy" | "toolset" | "bundle"
export interface ArtifactRef { readonly kind: ArtifactKind; readonly digest: string }
export interface ArtifactManifest { readonly kind: ArtifactKind; readonly payload_digest: string; readonly parents: readonly string[]; readonly toolchain_digest: string; readonly scope: string }
export interface ArtifactVersion { readonly digest: string; readonly manifest: ArtifactManifest }
export interface ArtifactSet { readonly digest: string; readonly artifacts: readonly ArtifactRef[] }
export interface EvolutionProposal { readonly digest: string; readonly base_artifact_set: string; readonly candidate_artifact_set: string; readonly objective: string; readonly change_manifest: string; readonly proposer: string; readonly constraints: readonly string[] }
export interface EvaluationContextBinding { readonly digest: string; readonly operation_id: string; readonly execution_input: string; readonly context_state: string; readonly context_policy: string; readonly context_plan: string; readonly rendered_snapshot: string; readonly prompt_measurement: string; readonly provider_route: string; readonly cache_prefix?: string }
export type ContextEntrySource = "system" | "knowledge" | "history" | "state" | "signal"
export type ContextPlanAction = "include" | "excerpt" | "collapse" | "page_out" | "omit"
export interface ContextEntryRef { readonly entry_id: string; readonly content_digest: string; readonly source: ContextEntrySource; readonly ordinal: number }
export interface ContextState { readonly schema: "context/v1"; readonly generation: number; readonly system: readonly ContextEntryRef[]; readonly knowledge: readonly ContextEntryRef[]; readonly history: readonly ContextEntryRef[]; readonly state: readonly ContextEntryRef[]; readonly task_state: string; readonly signals: readonly string[]; readonly digest: string }
export interface ContextSelection { readonly entry_id: string; readonly action: ContextPlanAction; readonly reason: string }
export interface ContextPlan { readonly schema: "context/v1"; readonly plan_id: string; readonly operation_id: string; readonly step_id: string; readonly state_digest: string; readonly state_generation: number; readonly runtime_inputs: string; readonly policy_digest: string; readonly provider_profile_digest: string; readonly measurement_fingerprints: readonly string[]; readonly selections: readonly ContextSelection[]; readonly input_budget_tokens: number; readonly projected_tokens: number; readonly pressure_ppm: number; readonly cache_prefix: { readonly digest: string; readonly entries: number } | null }
export interface ContextExecutionInput { readonly schema: "context/v1"; readonly input_digest: string; readonly operation_id: string; readonly step_id: string; readonly input_sequence: number; readonly state_digest: string; readonly policy_digest: string; readonly plan_digest: string; readonly rendered_snapshot: string; readonly prompt_measurement: string; readonly provider_route: string; readonly cache_prefix: { readonly digest: string; readonly entries: number } | null }
export interface ContextPreparationRequest { readonly operation_id: string; readonly step_id: string; readonly input_sequence: number; readonly policy_digest: string; readonly prompt_measurement: string; readonly provider_route: string }
export interface EvaluationRun { readonly digest: string; readonly proposal: string; readonly baseline_artifact_set: string; readonly candidate_artifact_set: string; readonly evaluator: string; readonly dataset: string; readonly operation_ids: readonly string[]; readonly contexts: readonly EvaluationContextBinding[]; readonly evidence_refs: readonly string[] }
export interface EvaluationMetric { readonly name: string; readonly baseline: string; readonly candidate: string; readonly improved: boolean }
export interface EvaluationGate { readonly name: string; readonly required: boolean; readonly passed: boolean }
export interface EvaluationFact { readonly digest: string; readonly evaluation: string; readonly metrics: readonly EvaluationMetric[]; readonly gates: readonly EvaluationGate[]; readonly replay_passed: boolean }
export type PromotionOutcome = "promote" | "reject" | "hold"
export interface PromotionDecision { readonly digest: string; readonly proposal: string; readonly evaluation_facts: readonly string[]; readonly policy: string; readonly outcome: PromotionOutcome; readonly selected_artifact_set: string }
export interface ActivationBinding { readonly digest: string; readonly operation_id: string; readonly artifact_set: string; readonly promotion_decision: string }
export interface EvolutionBundle { readonly artifacts: readonly ArtifactVersion[]; readonly artifact_sets: readonly ArtifactSet[]; readonly proposals: readonly EvolutionProposal[]; readonly evaluations: readonly EvaluationRun[]; readonly facts: readonly EvaluationFact[]; readonly decisions: readonly PromotionDecision[]; readonly activations: readonly ActivationBinding[] }
export type EvolutionVerdict = "pass" | "fail" | "unavailable"
export interface EvolutionReport { readonly schema: "evolution-report/v1"; readonly verdict: EvolutionVerdict; readonly violations: readonly { readonly code: string; readonly detail: string }[] }
export type EvolutionValidateJson = (request: string) => string
export interface EvolutionStore { loadBundle(): Promise<EvolutionBundle> | EvolutionBundle }

export function createEvolutionRuntimeAdapter(validateJson: EvolutionValidateJson) {
  return { validate: (bundle: EvolutionBundle): EvolutionReport => JSON.parse(validateJson(JSON.stringify(bundle))) as EvolutionReport }
}

export async function createNativeEvolutionRuntimeAdapter() {
  const kernel = await getKernel()
  return createEvolutionRuntimeAdapter(kernel.evolutionValidateJson)
}

export class EvolutionRuntime {
  constructor(private readonly adapter: ReturnType<typeof createEvolutionRuntimeAdapter>) {}
  validate(bundle: EvolutionBundle): EvolutionReport { return this.adapter.validate(bundle) }
  async validateStore(store: EvolutionStore): Promise<EvolutionReport> { return this.validate(await store.loadBundle()) }
  activate(bundle: EvolutionBundle, operationId: string): ActivationBinding {
    const report = this.validate(bundle)
    if (report.verdict !== "pass") throw new Error(`evolution bundle is not activatable: ${report.violations.map(v => v.code).join(", ")}`)
    const activation = bundle.activations.find(value => value.operation_id === operationId)
    if (!activation) throw new Error(`no verified activation binding for operation ${operationId}`)
    return activation
  }
}
