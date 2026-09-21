/** SPC-028-02: cross-layer object classification registry. */
export type RuntimeDomain = "public" | "host" | "kernel" | "provider"
export type RuntimeAuthority = "public-agent" | "host-runtime" | "kernel" | "none"
export type RuntimeRepresentation = "semantic" | "projection" | "measurement" | "evidence" | "wire" | "state-truth" | "snapshot"
export type RuntimeDurability = "durable" | "ephemeral" | "rebuildable" | "append-only"

export interface RuntimeObjectClassification {
  domain: RuntimeDomain
  authority: RuntimeAuthority
  representation: RuntimeRepresentation
  durability: RuntimeDurability
  identity: string
  causation: string
  replay: string
}

export const RUNTIME_OBJECT_CLASSIFICATIONS = {
  AgentDefinition: {
    domain: "public", authority: "public-agent", representation: "semantic", durability: "rebuildable",
    identity: "agent name or host-assigned agent identity", causation: "agent declaration", replay: "input to AgentSpec projection",
  },
  AgentSpec: {
    domain: "host", authority: "host-runtime", representation: "semantic", durability: "rebuildable",
    identity: "agent identity", causation: "AgentDefinition plus host bindings", replay: "re-derived from public definition and runtime bindings",
  },
  StoredMessageState: {
    domain: "host", authority: "host-runtime", representation: "state-truth", durability: "durable",
    identity: "message identity", causation: "accepted session or run input", replay: "replayed from durable message history",
  },
  ProviderRequestPlan: {
    domain: "host", authority: "none", representation: "projection", durability: "rebuildable",
    identity: "request fingerprint", causation: "ContextCandidate plus ResolvedProviderRoute", replay: "re-rendered and re-encoded from inputs",
  },
  PromptMeasurement: {
    domain: "host", authority: "host-runtime", representation: "measurement", durability: "durable",
    identity: "request fingerprint", causation: "prepared provider request", replay: "reused only on exact fingerprint match",
  },
  ProviderUsage: {
    domain: "provider", authority: "host-runtime", representation: "measurement", durability: "durable",
    identity: "provider attempt identity", causation: "decoded provider response", replay: "retained as response evidence",
  },
  ResolvedProviderRoute: {
    domain: "provider", authority: "host-runtime", representation: "projection", durability: "durable",
    identity: "route identity", causation: "model resolution", replay: "frozen on ProviderAttempt",
  },
  ProviderAttempt: {
    domain: "host", authority: "host-runtime", representation: "evidence", durability: "append-only",
    identity: "effect identity plus attempt sequence", causation: "CallProvider effect", replay: "evidence for one physical execution",
  },
  KernelInput: {
    domain: "kernel", authority: "kernel", representation: "wire", durability: "append-only",
    identity: "input identity", causation: "host submission", replay: "durable input sequence",
  },
  KernelEffect: {
    domain: "kernel", authority: "kernel", representation: "wire", durability: "append-only",
    identity: "effect identity", causation: "kernel decision", replay: "journal decision chain",
  },
  BudgetLedger: {
    domain: "kernel", authority: "kernel", representation: "state-truth", durability: "durable",
    identity: "operation or group budget identity", causation: "accepted kernel facts", replay: "folded from journal facts",
  },
  Journal: {
    domain: "kernel", authority: "kernel", representation: "state-truth", durability: "append-only",
    identity: "journal sequence and digest", causation: "kernel inputs and facts", replay: "source of durable state truth",
  },
  Checkpoint: {
    domain: "kernel", authority: "kernel", representation: "snapshot", durability: "durable",
    identity: "checkpoint identity and journal head", causation: "checkpoint boundary", replay: "restore seed verified against journal",
  },
  SessionLog: {
    domain: "host", authority: "none", representation: "evidence", durability: "append-only",
    identity: "session identity and event sequence", causation: "host observations", replay: "evidence only; never recovery authority",
  },
  EvaluationRun: {
    domain: "host", authority: "host-runtime", representation: "semantic", durability: "durable",
    identity: "evaluation run identity", causation: "public Eval request", replay: "replayed from dataset and captured evidence",
  },
} as const satisfies Record<string, RuntimeObjectClassification>

export type RuntimeObjectName = keyof typeof RUNTIME_OBJECT_CLASSIFICATIONS
