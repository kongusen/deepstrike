import { getKernel } from "../kernel.js"
import type { RecordedPromptMeasurement, ResolvedProviderRoute } from "../providers/request-plan.js"
import type { ContextExecutionInput, ContextPlan, ContextState, EvaluationContextBinding } from "./evolution.js"

/** Host evidence completing the kernel's immutable context candidate. */
export interface ContextProviderPreparationRequest {
  readonly effect: Record<string, unknown>
  readonly request_fingerprint: string
  readonly provider_route: ResolvedProviderRoute
  readonly prompt_measurement: RecordedPromptMeasurement
}
export interface ContextPrepared {
  readonly state: ContextState
  readonly provider_route: ResolvedProviderRoute
  readonly prompt_measurement: RecordedPromptMeasurement
  readonly execution_input: ContextExecutionInput
  readonly plan: ContextPlan
  readonly binding: EvaluationContextBinding
}
export type ContextPrepareJson = (request: string) => string
export type ContextVerifyJson = (request: string) => string

/** All validation and content addressing are delegated to the Rust authority. */
export function createContextPreparationAdapter(prepareJson: ContextPrepareJson, verifyJson: ContextVerifyJson) {
  return {
    verify(effect: Record<string, unknown>, preparation: ContextPrepared): boolean {
      return JSON.parse(verifyJson(JSON.stringify({ effect, preparation }))) as boolean
    },
    prepare(request: ContextProviderPreparationRequest): ContextPrepared {
      return JSON.parse(prepareJson(JSON.stringify(request))) as ContextPrepared
    },
  }
}

export function createNativeContextPreparationAdapter() {
  const kernel = getKernel()
  return createContextPreparationAdapter(kernel.contextPrepareJson, kernel.contextVerifyJson)
}
