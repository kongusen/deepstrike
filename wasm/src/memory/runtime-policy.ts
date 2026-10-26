// Host-facing memory runtime policy (contract M1). Each capability is an independent switch: a
// host can keep retrieval while turning off the auxiliary-model writes that run implicitly after a
// session. `"disabled"` is checked before any auxiliary provider call, so a disabled path costs
// nothing. `"candidate_sink"` keeps the path on but hands each write to the host's
// `MemoryCandidateSink` (M2) instead of the memory store; the runner refuses it without a sink.

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
  /** The path is on and its writes go to the candidate sink, not the memory store. */
  sessionExtractionToCandidateSink: boolean
  modelWritesToCandidateSink: boolean
  semanticArchiveToCandidateSink: boolean
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
  }
  return {
    retrievalEnabled: policy.retrieval !== "disabled",
    sessionExtractionEnabled: policy.sessionExtraction !== "disabled",
    modelWritesEnabled: policy.modelWrites !== "disabled",
    semanticArchiveEnabled: policy.semanticArchive !== "disabled",
    sessionExtractionToCandidateSink: policy.sessionExtraction === "candidate_sink",
    modelWritesToCandidateSink: policy.modelWrites === "candidate_sink",
    semanticArchiveToCandidateSink: policy.semanticArchive === "candidate_sink",
  }
}
