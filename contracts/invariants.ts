/** Cross-boundary invariants that relate multiple effects/events and cannot be expressed as fields. */

export const GLOBAL_INVARIANTS = [
  {
    id: "effect-id-kernel-minted",
    scope: "operation",
    rule: "Every host evidence record carrying effect_id must reference a kernel-minted pending effect; hosts may propagate an id but never mint a replacement.",
    enforcement: "canonical-kernel-and-session-log",
    testRefs: [{ path: "node/tests/effect-hygiene.test.ts", selectors: ["R-B27: an unverifiable milestone effect is still resolved"] }, { path: "node/tests/canonical-binding.test.ts", selectors: ["CanonicalKernel native binding"] }],
  },
  {
    id: "signal-disposal-one-to-one",
    scope: "operation",
    rule: "Each (delivery_id, attempt) has exactly one signal_delivery_disposed receipt before ack or nack completes.",
    enforcement: "canonical-kernel-and-signal-drain",
    testRefs: [{ path: "node/tests/signal-boundary.test.ts", selectors: ["signal host-to-kernel boundary"] }, { path: "node/tests/signal-addressing.test.ts", selectors: ["SignalGateway recipient addressing (R1/L0)"] }],
  },
  {
    id: "run-context-isolation",
    scope: "run-session-operation",
    rule: "Evidence, background work, and callbacks retain the originating runId and sessionId; a receipt from another operation cannot resolve the current pending effect.",
    enforcement: "OperationContext-and-session-log",
    testRefs: [{ path: "node/tests/runtime/reliability.test.ts", selectors: ["ManagedTaskScope"] }, { path: "node/tests/agent-session-isolation.test.ts", selectors: ["Agent facade session isolation"] }],
  },
] as const
