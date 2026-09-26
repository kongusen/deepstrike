/** Cross-boundary invariants that relate multiple effects/events and cannot be expressed as fields. */

export const GLOBAL_INVARIANTS = [
  {
    id: "effect-id-kernel-minted",
    scope: "operation",
    rule: "Every host evidence record carrying effect_id must reference a kernel-minted pending effect; hosts may propagate an id but never mint a replacement.",
    enforcement: "canonical-kernel-and-session-log",
    testRefs: [{ path: "node/tests/boundary-p1-regressions.test.ts", selectors: ["refuses to acknowledge an effect id that is not a pending launch", "never invents an attempt id for a completion it cannot attribute"] }, { path: "node/tests/boundary-p2-regressions.test.ts", selectors: ["does not advance the turn or record messages for a provider result the kernel refused"] }],
  },
  {
    id: "signal-disposal-one-to-one",
    scope: "operation",
    rule: "Each (delivery_id, attempt) has exactly one signal_delivery_disposed receipt before ack or nack completes.",
    enforcement: "canonical-kernel-and-signal-drain",
    testRefs: [{ path: "node/tests/boundary-p1-regressions.test.ts", selectors: ["admits a deadline-bearing signal and a host note through the real kernel"] }, { path: "node/tests/signal-boundary.test.ts", selectors: ["signal host-to-kernel boundary"] }],
  },
  {
    id: "run-context-isolation",
    scope: "run-session-operation",
    rule: "Evidence, background work, and callbacks retain the originating runId and sessionId; a receipt from another operation cannot resolve the current pending effect.",
    enforcement: "OperationContext-and-session-log",
    testRefs: [{ path: "node/tests/boundary-p2-regressions.test.ts", selectors: ["refuses a receipt addressed from another operation"] }, { path: "node/tests/agent-session-isolation.test.ts", selectors: ["Agent facade session isolation"] }],
  },
] as const
