/**
 * Type-driven boundary contract system for semantic crossings between architectural layers.
 *
 * Design principles:
 * - A boundary protocol describes a layer relationship, not individual functions
 * - Type structure drives inference of preserves/drops
 * - Security policy (forbidden fields) remains explicit
 * - Generated manifests are outputs, not sources of truth
 */

/**
 * Four protocol families covering all semantic crossings in the runtime.
 */
export type ProtocolFamily =
  | "public-to-host"    // User-facing API → Runtime execution (Agent → AgentSpec)
  | "host-to-kernel"    // Runtime → Kernel ABI (SkillMetadata → KernelSkillMetadata)
  | "kernel-to-host"    // Kernel observations → Host event log (KernelObservation → SessionEvent)
  | "host-provider"     // Runtime ↔ Vendor wire (ProviderAttempt ↔ VendorRequest)
  | "runtime-internal"  // Host layer transformations (SkillRevision → SkillPackage)

/**
 * Direction of a protocol crossing.
 */
export type CrossingDirection =
  | "lower"      // Public to host (Agent → AgentSpec)
  | "project"    // Host to kernel (SkillMetadata → kernel projection)
  | "encode"     // Host to provider (ProviderAttempt → VendorRequest)
  | "decode"     // Provider to host (VendorResponse → Evidence)
  | "materialize" // Runtime internal transformation

/**
 * Field preservation policy for a boundary crossing.
 */
export interface FieldPolicy {
  /**
   * Fields that cross the boundary unchanged (same name, type-compatible).
   * Inferred from type overlap when not explicitly declared.
   */
  preserves?: readonly string[]

  /**
   * Fields that are renamed across the boundary.
   * Example: { estimatedTokens: "estimated_tokens" }
   */
  renames?: Readonly<Record<string, string>>

  /**
   * Fields in the target that are computed/derived, not preserved.
   * These exist in the target but not the source.
   */
  derived?: readonly string[]

  /**
   * Fields that are intentionally dropped (present in source, absent in target).
   * Inferred as (source keys - preserves - renames.keys).
   */
  drops?: readonly string[]

  /**
   * Fields that MUST NOT cross this boundary (security/encapsulation policy).
   * Runtime validator rejects if any forbidden field appears in the result.
   *
   * Examples:
   * - provider_credentials (never to kernel)
   * - storage_backend (implementation detail)
   * - activation_authority (kernel-only state)
   */
  forbidden: readonly string[]
}

/**
 * Lazy resource loading policy (Anthropic skill protocol compatibility).
 *
 * Skills support progressive disclosure: metadata crosses early, content on-demand.
 */
export interface LazyLoadingPolicy {
  /**
   * Fields that represent resource references (loaded on-demand).
   * Example: ["instructions", "resource_contents"]
   */
  lazyFields?: readonly string[]

  /**
   * Whether this crossing preserves lazy semantics.
   * - "preserve": Target maintains resource references, no eager load
   * - "materialize": Target forces resolution (e.g., kernel needs instructions now)
   * - "none": No lazy fields in this crossing
   */
  lazySemantics: "preserve" | "materialize" | "none"
}

export interface BoundaryEndpoint {
  type: string
  layer: "public" | "host" | "kernel" | "provider"
  authority: "public-agent" | "host-runtime" | "kernel" | "provider"
}

export interface BoundaryArtifacts {
  manifest: string
  validator?: {
    path: string
    exportName: string
    predicateName: string
    targetImport: string
    targetType: string
    label: string
  }
}

/** One typed implementation inside a protocol family. */
export interface BoundaryAdapter {
  adapter: string
  source: BoundaryEndpoint
  target: BoundaryEndpoint
  fields?: FieldPolicy
  artifacts?: BoundaryArtifacts
}

/**
 * A boundary protocol declaration.
 *
 * Generic parameters are nominal type names for documentation and manifest generation.
 * The actual type checking happens via the typed adapter signature.
 */
export interface BoundaryProtocol<Source = string, Target = string> {
  /** Stable protocol identity used by the registry and generated artifacts. */
  id: string

  /** Protocol family this crossing belongs to. */
  family: ProtocolFamily

  /** Direction verb for this specific crossing. */
  direction: CrossingDirection

  /** Source type name (for manifest generation). */
  source?: BoundaryEndpoint & { type: Source extends string ? Source : string }

  /** Target type name (for manifest generation). */
  target?: BoundaryEndpoint & { type: Target extends string ? Target : string }

  /** Field preservation and security policy. */
  fields: FieldPolicy

  /** Lazy loading policy (Anthropic skill protocol). */
  lazy: LazyLoadingPolicy

  /**
   * Adapter function that implements this crossing.
   * Format: "modulePath:functionName" or just "functionName" for well-known adapters.
   * Example: "runtime/kernel-step:skillMetadataToKernel"
   */
  adapter?: string

  /** Generated artifact destinations for this protocol. Paths are repository-relative. */
  artifacts?: BoundaryArtifacts

  /** Multiple typed adapters that share this protocol family and policy. */
  adapters?: readonly BoundaryAdapter[]

  /**
   * Whether this crossing is lossy by design.
   * "intentional" means drops are expected; "lossless" means source ≈ target.
   */
  lossiness: "intentional" | "lossless"
}

/**
 * Registry of all boundary protocols in the system.
 */
export interface ProtocolRegistry {
  /** Semantic version of the contract system. */
  version: string

  /** All registered boundary protocols. */
  protocols: ReadonlyArray<BoundaryProtocol>
}

/**
 * Generated crossing manifest (output artifact, not source of truth).
 */
export interface CrossingManifest {
  /** Unique identifier for this crossing. */
  id: string

  /** Source protocol this manifest was generated from. */
  protocol: BoundaryProtocol

  /** Actual inferred preserves (computed from types). */
  inferredPreserves: readonly string[]

  /** Actual inferred drops (computed from types). */
  inferredDrops: readonly string[]

  /** Adapter function signature (resolved from TypeScript compiler). */
  adapterSignature: {
    parameters: Array<{ name: string; type: string }>
    returnType: string
  }

  /** Timestamp of manifest generation. */
  generatedAt: string
}
