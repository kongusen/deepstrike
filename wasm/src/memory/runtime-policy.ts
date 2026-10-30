// Host-facing memory runtime policy (contract M1). Each capability is an independent switch: a
// host can keep retrieval while turning off model-authored writes. `"disabled"` turns a path off;
// `"candidate_sink"` keeps writes on but hands them to the host's `MemoryCandidateSink` (M2).

export type MemoryRetrievalPolicy = "disabled" | "host"
export type MemoryWritePolicy = "disabled" | "candidate_sink"

export interface MemoryRuntimePolicy {
  retrieval: MemoryRetrievalPolicy
  modelWrites: MemoryWritePolicy
}

export interface ResolvedMemoryRuntimePolicy {
  retrievalEnabled: boolean
  modelWritesEnabled: boolean
  /** The path is on and its writes go to the candidate sink, not the memory store. */
  modelWritesToCandidateSink: boolean
}

const POLICY_FIELDS = ["retrieval", "modelWrites"] as const

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
  }
  return {
    retrievalEnabled: policy.retrieval !== "disabled",
    modelWritesEnabled: policy.modelWrites !== "disabled",
    modelWritesToCandidateSink: policy.modelWrites === "candidate_sink",
  }
}
