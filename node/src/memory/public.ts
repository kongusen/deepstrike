// `@deepstrike/sdk/memory` — long-term (dream) and working memory, plus the knowledge-source interface.
export { WorkingMemory } from "./working.js"
export { InMemoryDreamStore } from "./in-memory-store.js"
export type { InMemoryDreamStoreOptions } from "./in-memory-store.js"
export { memoryRetentionScore } from "./retention.js"
export { rankMemories } from "./ranking.js"
export type { RankableMemory, RankedMemory, RankOptions } from "./ranking.js"
export { extractSessionMemories, parseExtractedMemories } from "./extraction.js"
export type {
  DreamStore, SessionData, SessionMessage, MemoryRecord, MemoryRecall,
  MemoryQuery, MemoryScope, MemoryProvenance, MemoryRecallLifecycle,
  MemoryKind, MemoryAuthor, MemoryTrustLevel,
} from "./protocols.js"
export type { KnowledgeSource } from "../knowledge/source.js"
