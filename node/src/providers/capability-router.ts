import type { CreateProviderOptions } from "./catalog.js"
import { resolveProviderRuntimeAsync } from "./catalog.js"
import type { InputModality, ResolvedProviderRuntime } from "./model-registry.js"

export interface CapabilityRequirement {
  requiredInputModalities?: readonly InputModality[]
  tools?: boolean
  reasoning?: boolean
  minimumContextWindow?: number
}

export type CapabilityRouteResult =
  | { ok: true; runtime: ResolvedProviderRuntime }
  | {
    ok: false
    error: {
      code: "no_capable_model"
      requirement: CapabilityRequirement
      candidates: Array<{ model: string; rejectedBy: string[] }>
    }
  }

/**
 * Deliberately policy-free first-match router. Unknown capability evidence stays eligible for
 * forward compatibility; only evidence of unsupported capability rejects a candidate.
 */
export class CapabilityRouter {
  async route(
    requirement: CapabilityRequirement,
    candidates: readonly CreateProviderOptions[],
  ): Promise<CapabilityRouteResult> {
    const rejected: Array<{ model: string; rejectedBy: string[] }> = []
    for (const candidate of candidates) {
      let runtime: ResolvedProviderRuntime
      try {
        runtime = await resolveProviderRuntimeAsync(candidate)
      } catch {
        rejected.push({ model: candidate.model, rejectedBy: ["unavailable"] })
        continue
      }
      const rejectedBy = rejects(requirement, runtime)
      if (rejectedBy.length === 0) return { ok: true, runtime }
      rejected.push({ model: candidate.model, rejectedBy })
    }
    return { ok: false, error: { code: "no_capable_model", requirement, candidates: rejected } }
  }
}

function rejects(requirement: CapabilityRequirement, runtime: ResolvedProviderRuntime): string[] {
  const result: string[] = []
  for (const modality of requirement.requiredInputModalities ?? []) {
    if (runtime.effectiveCapabilities.inputModalities[modality].state === "unsupported") {
      result.push(`input:${modality}`)
    }
  }
  if (requirement.tools && runtime.effectiveCapabilities.tools.state === "unsupported") result.push("tools")
  if (requirement.reasoning && runtime.effectiveCapabilities.reasoning.state === "unsupported") result.push("reasoning")
  if (
    requirement.minimumContextWindow !== undefined
    && runtime.model.contextWindow !== undefined
    && runtime.model.contextWindow < requirement.minimumContextWindow
  ) {
    result.push("context")
  }
  return result
}
