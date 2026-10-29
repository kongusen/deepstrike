// `@deepstrike/sdk/memory` — durable and working memory, plus the knowledge-source interface.
export { WorkingMemory } from "./working.js"
export { DurableMemory } from "./durable.js"
export { InMemoryMemoryStore } from "./in-memory-store.js"
export type { InMemoryMemoryStoreOptions } from "./in-memory-store.js"
export { memoryRetentionScore } from "./retention.js"
export { rankMemories } from "./ranking.js"
export type { RankableMemory, RankedMemory, RankOptions } from "./ranking.js"
export { extractSessionMemories, parseExtractedMemories } from "./extraction.js"
export { resolveMemoryRuntimePolicy } from "./runtime-policy.js"
export { InMemoryMemoryCandidateSink, memoryCandidateId, memoryCandidatePayloadDigest, sessionExtractionCandidateId } from "./candidate-sink.js"
export { FileMemoryCandidateSink } from "./durable-candidate-sink.js"
export { Ed25519MemoryCandidateSourceVerifier, HmacMemoryCandidateSourceVerifier, admitMemoryCandidate, memoryCandidateAttestationCanonicalPayload, memoryCandidateAttestationDigest, signMemoryCandidateAttestation, verifyMemoryCandidateSource } from "./attestation.js"
export type { Ed25519MemoryCandidateSourceVerifierOptions, MemoryCandidateAttestation, MemoryCandidateAttestationReasonCode, MemoryCandidateSourceVerifier, MemoryCandidateVerification, HmacMemoryCandidateSourceVerifierOptions } from "./attestation.js"
export type { FileMemoryCandidateSinkOptions, MemoryCandidateLedgerEntry, MemoryCandidateOutboxEvent, MemoryCandidateResolution, MemoryCandidateVerificationAttempt } from "./durable-candidate-sink.js"
export { memoryRecallTurn } from "./recall-projection.js"
export { consumeMemoryLeaseWrite, memoryRecordExpired, memoryScopeMatches, revokeMemoryLease, scopeMatchesMemoryDeletion, verifyMemoryLease } from "./lifecycle.js"
export type { MemoryLease, MemoryLeaseRejection, MemoryWriteBudget } from "./lifecycle.js"
export type {
  MemoryCandidate, MemoryCandidateReceipt, MemoryCandidateSink, MemoryCandidateSource,
} from "./candidate-sink.js"
export type { MemoryRuntimePolicy, MemoryRetrievalPolicy, MemoryWritePolicy, ResolvedMemoryRuntimePolicy } from "./runtime-policy.js"
export type {
  MemoryStore, Memory, MemorySearchOptions, SessionData, SessionMessage, MemoryRecord, MemoryRecall,
  MemoryQuery, MemoryScope, MemoryProvenance, MemoryRecallLifecycle,
  MemoryKind, MemoryAuthor, MemoryTrustLevel,
} from "./protocols.js"
export type { KnowledgeSource } from "../knowledge/source.js"
