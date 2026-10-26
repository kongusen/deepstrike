// Host-facing memory runtime policy (contract M1). Each capability is an independent switch: a
// host can keep retrieval while turning off the auxiliary-model writes that run implicitly after a
// session. `"disabled"` is checked before any auxiliary provider call, so a disabled path costs
// nothing. `"candidate_sink"` needs the candidate hand-off protocol (M2) and is rejected for now.

export type MemoryRetrievalPolicy = "disabled" | "host"
export type MemoryWritePolicy = "disabled" | "candidate_sink"

export interface MemoryRuntimePolicy {
  retrieval: MemoryRetrievalPolicy
  sessionExtraction: MemoryWritePolicy
  modelWrites: MemoryWritePolicy
  semanticArchive: MemoryWritePolicy
}

export interface ResolvedMemoryRuntimePolicy {
  retrievalEnabled: boolean
  sessionExtractionEnabled: boolean
  modelWritesEnabled: boolean
  semanticArchiveEnabled: boolean
}

const POLICY_FIELDS = ["retrieval", "sessionExtraction", "modelWrites", "semanticArchive"] as const

/** Validate a host-supplied policy and project it onto runner gates. `undefined` keeps the
 *  existing behavior: every memory path stays enabled. */
export function resolveMemoryRuntimePolicy(policy: MemoryRuntimePolicy | undefined): ResolvedMemoryRuntimePolicy | undefined {
  if (policy === undefined) return undefined
  const unknown = Object.keys(policy).filter(key => !(POLICY_FIELDS as readonly string[]).includes(key))
  if (unknown.length > 0) {
    throw new TypeError(`unknown memory runtime policy field(s): ${unknown.join(", ")}`)
  }
  for (const field of POLICY_FIELDS) {
    const value: string | undefined = policy[field]
    const allowed: readonly string[] = field === "retrieval" ? ["disabled", "host"] : ["disabled", "candidate_sink"]
    if (value === undefined || !allowed.includes(value)) {
      throw new TypeError(`memory runtime policy ${field} must be one of ${allowed.join(" | ")}, got ${String(value)}`)
    }
    if (value === "candidate_sink") {
      throw new Error(
        `memory runtime policy ${field} = "candidate_sink" needs the memory candidate sink, which is not available in this SDK version`,
      )
    }
  }
  return {
    retrievalEnabled: policy.retrieval !== "disabled",
    sessionExtractionEnabled: policy.sessionExtraction !== "disabled",
    modelWritesEnabled: policy.modelWrites !== "disabled",
    semanticArchiveEnabled: policy.semanticArchive !== "disabled",
  }
}
