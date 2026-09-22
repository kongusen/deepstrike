/** Generated from contracts/vocabulary.json. Do not edit by hand. */
export const RUNTIME_VOCABULARY = {
  "version": "0.2.74",
  "public": [
    "Agent",
    "Model",
    "Run",
    "Session",
    "Tool",
    "Skill",
    "Memory",
    "Knowledge",
    "MCPServer",
    "Handoff",
    "Workflow",
    "Guardrail",
    "Eval",
    "Dataset",
    "Evaluator",
    "Output",
    "Usage"
  ],
  "host": [
    "AgentSpec",
    "Context",
    "ContextPlan",
    "Capability",
    "ModelRoute",
    "Invocation",
    "ProviderAttempt",
    "Measurement",
    "Evidence",
    "Artifact",
    "Evaluation",
    "Promotion",
    "ExecutionPlane"
  ],
  "kernel": [
    "Operation",
    "Intent",
    "Decision",
    "Effect",
    "Fact",
    "Settlement",
    "Task",
    "Capability",
    "Budget",
    "Journal",
    "Checkpoint",
    "StateTransition"
  ],
  "provider": [
    "Model",
    "Provider",
    "Endpoint",
    "Protocol",
    "Route",
    "Adapter",
    "Request",
    "Response",
    "Usage",
    "ReplayEvidence"
  ],
  "primaryDomain": {
    "Model": "public",
    "Usage": "public",
    "Capability": "host",
    "Route": "host"
  },
  "verbs": {
    "bind": "establish and freeze an identity relationship",
    "decode": "extract semantic output and retained wire evidence",
    "encode": "map semantic input into a wire request",
    "lower": "convert a public declaration into host runtime form",
    "normalize": "map vendor data into the controlled runtime vocabulary",
    "prepare": "materialize a validated runtime preparation through the kernel adapter",
    "project": "produce a read-only representation for another authority",
    "resolve": "select or answer a runtime object at a boundary",
    "render": "project semantic state into model-facing input",
    "settle": "apply accounting policy to an observed measurement"
  }
} as const

export type RuntimeLanguage = typeof RUNTIME_VOCABULARY
export type RuntimeLanguageDomain = "public" | "host" | "kernel" | "provider"

export function runtimeVocabularyTerms(): readonly string[] {
  return [
    ...RUNTIME_VOCABULARY.public,
    ...RUNTIME_VOCABULARY.host,
    ...RUNTIME_VOCABULARY.kernel,
    ...RUNTIME_VOCABULARY.provider,
  ]
}
