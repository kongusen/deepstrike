import { getKernel } from "./kernel.js"

export type ArtifactKind = "runtime" | "skill" | "prompt" | "policy" | "toolset" | "bundle"
export interface ArtifactRef { readonly kind: ArtifactKind; readonly digest: string }
export interface ArtifactManifest { readonly kind: ArtifactKind; readonly payload_digest: string; readonly parents: readonly string[]; readonly toolchain_digest: string; readonly scope: string }
export interface ArtifactVersion { readonly digest: string; readonly manifest: ArtifactManifest }
export interface ArtifactSet { readonly digest: string; readonly artifacts: readonly ArtifactRef[] }
export interface EvolutionProposal { readonly digest: string; readonly base_artifact_set: string; readonly candidate_artifact_set: string; readonly objective: string; readonly change_manifest: string; readonly proposer: string; readonly constraints: readonly string[] }
export interface EvaluationRun { readonly digest: string; readonly proposal: string; readonly baseline_artifact_set: string; readonly candidate_artifact_set: string; readonly evaluator: string; readonly dataset: string; readonly operation_ids: readonly string[]; readonly evidence_refs: readonly string[] }
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
}
