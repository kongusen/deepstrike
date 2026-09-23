/**
 * SPC-028-01: the normative vocabulary for the four runtime language layers.
 *
 * Terms are names, not a second semantic authority. The registry is consumed by
 * conformance tests and documentation tooling so each term has one primary layer.
 */
export const RUNTIME_VOCABULARY = {
  version: "0.2.73",
  public: [
    "Agent", "Model", "Run", "Session", "Tool", "Skill", "Memory", "Knowledge",
    "MCPServer", "Handoff", "Workflow", "Guardrail", "Eval", "Dataset", "Evaluator",
    "Output", "Usage",
  ],
  host: [
    "Context", "ContextPlan", "Capability", "ModelRoute", "Invocation",
    "ProviderAttempt", "Measurement", "Evidence", "Artifact", "Evaluation", "Promotion",
    "ExecutionPlane",
  ],
  kernel: [
    "Operation", "Intent", "Decision", "Effect", "Fact", "Settlement", "Task", "Capability",
    "Budget", "Journal", "Checkpoint", "StateTransition",
  ],
  provider: [
    "Model", "Provider", "Endpoint", "Protocol", "Route", "Adapter", "Request", "Response",
    "Usage", "ReplayEvidence",
  ],
  primaryDomain: {
    Model: "public",
    Usage: "public",
    Capability: "host",
    Route: "host",
  },
  verbs: {
    resolve: "select or answer a runtime object at a boundary",
    render: "project semantic state into model-facing input",
    encode: "map semantic input into a wire request",
    execute: "perform one physical provider or external call",
    decode: "extract semantic output and retained wire evidence",
    normalize: "map vendor data into the controlled runtime vocabulary",
    settle: "apply accounting policy to an observed measurement",
  },
} as const

export type RuntimeLanguage = typeof RUNTIME_VOCABULARY
export type RuntimeLanguageDomain = "public" | "host" | "kernel" | "provider"

/** Returns all layer terms while preserving the registry's primary-domain order. */
export function runtimeVocabularyTerms(): readonly string[] {
  return [
    ...RUNTIME_VOCABULARY.public,
    ...RUNTIME_VOCABULARY.host,
    ...RUNTIME_VOCABULARY.kernel,
    ...RUNTIME_VOCABULARY.provider,
  ]
}
