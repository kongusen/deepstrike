// Public reaction checkpoint contract. The implementation remains behind the
// runtime boundary while this module gives SDK users a stable host-owned store.
export {
  InMemoryReactionCheckpointStore,
} from "./runtime/reaction-checkpoint.js"
export type {
  ReactionCheckpointClaim,
  ReactionCheckpointClaimResult,
  ReactionCheckpointReceipt,
  ReactionCheckpointStore,
  InMemoryReactionCheckpointStoreOptions,
} from "./runtime/reaction-checkpoint.js"
