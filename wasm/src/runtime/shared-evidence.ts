import { sha256Hex } from "./sha256.js"

import type { MemoryCandidateReceipt } from "../memory/candidate-sink.js"
import type { MemoryScope } from "../memory/index.js"
import type { KernelJournal } from "./kernel-journal.js"
import type { SessionEvent, SessionLog } from "./session-log.js"
import type { VerifiableEvidence } from "./verifiable-report.js"

/** Domains whose references can be composed into one host-side evidence snapshot. */
export type EvidenceDomain =
  | "journal_record"
  | "session_event"
  | "checkpoint"
  | "context_binding"
  | "provider_attempt"
  | "prompt_measurement"
  | "wire_replay"
  | "knowledge_entry"
  | "memory_record"
  | "memory_candidate"

/** Who owns the fact represented by a reference. */
export type EvidenceAuthority = "kernel_fact" | "host_observation" | "derived_projection"

/** Availability is a read-model observation and is separate from kernel Residency. */
export type EvidenceAvailability = "resident" | "external" | "missing" | "redacted"

export type EvidenceCompleteness = "complete" | "partial" | "unavailable"

/** Where a memory write stands at read time; only memory domains carry it. */
export type MemorySettlement = "pending" | "accepted" | "rejected"

/** The host scope in which an evidence reference is meaningful. */
export interface EvidenceScope {
  readonly operationId: string
  readonly sessionId?: string
  readonly agentId?: string
  readonly tenantId?: string
  readonly namespace?: string
}

/** A bounded diagnostic produced while assembling or reading a snapshot. */
export interface EvidenceDiagnostic {
  readonly code: string
  readonly message: string
  readonly domain?: EvidenceDomain
  readonly refId?: string
}

/**
 * Optional lifecycle observations copied from the source domain. They do not become a shared
 * retention policy and are never written back to memory or knowledge retention fields.
 */
export interface EvidenceRetentionObservation {
  readonly pinned?: boolean
  /** The source's TTL as observed at read time; not a policy, unlike memory's `ttl_days`. */
  readonly observedTtlDays?: number
  readonly prunable?: boolean
}

/**
 * How one ref derives from another in the same snapshot:
 * - `recalled_from`: a knowledge entry is the in-context copy of a recalled memory record.
 * - `settled_as`: a memory candidate was accepted as this memory record.
 */
export type EvidenceRelation = "recalled_from" | "settled_as"

/** A domain-qualified edge to another ref; unlike `evidenceRefs`, it is resolved in the snapshot. */
export interface EvidenceLink {
  readonly relation: EvidenceRelation
  readonly domain: EvidenceDomain
  readonly refId: string
}

/**
 * Storage-neutral, domain-qualified evidence identity. The locator is opaque to this layer: it
 * may identify a host sidecar, but it is never interpreted as a path, database handle, or writer.
 */
export interface EvidenceRef {
  readonly refId: string
  readonly domain: EvidenceDomain
  readonly authority: EvidenceAuthority
  readonly scope: EvidenceScope
  readonly contentDigest?: string
  readonly payloadRef?: string
  readonly sizeBytes?: number
  readonly version?: string
  readonly availability: EvidenceAvailability
  readonly provenance?: string
  readonly retention?: EvidenceRetentionObservation
  readonly settlement?: MemorySettlement
  /**
   * Opaque ids of the evidence the source domain recorded this fact as derived from (e.g. memory
   * `provenance.evidence_refs`), in source order. They are not resolved against this snapshot.
   */
  readonly evidenceRefs?: readonly string[]
  /** Edges to other refs in this snapshot, sorted; a link whose target is absent is a gap. */
  readonly links?: readonly EvidenceLink[]
}

/**
 * The source positions a snapshot was read at. A field is present only when that source was read;
 * `-1` means the source was read and was empty (the `SessionLog.latestSeq` convention).
 */
export interface EvidenceCursor {
  readonly sessionLogSeq?: number
  readonly journalStepSeq?: number
  readonly checkpointOrdinal?: number
}

export interface SharedEvidenceIdentity {
  readonly operationId: string
  readonly scope: EvidenceScope
}

export interface SharedEvidenceFacets {
  /** Host-only references to execution observations; raw provider I/O stays in its source log. */
  readonly execution?: readonly EvidenceRef[]
  /**
   * Non-execution references: kernel records, checkpoints, knowledge and memory. Knowledge and
   * memory are composed here without changing their write semantics.
   */
  readonly references?: readonly EvidenceRef[]
}

export interface SharedEvidenceSnapshot {
  readonly identity: SharedEvidenceIdentity
  /** Where each source was read up to; absent when the caller did not supply one. */
  readonly asOf?: EvidenceCursor
  /**
   * Typed arrays cannot be frozen, so every read returns a fresh copy: writes to the returned
   * bytes never reach the snapshot. Hold the result in a local instead of re-reading it in a loop.
   */
  readonly verification: VerifiableEvidence
  readonly refs: readonly EvidenceRef[]
  readonly facets: SharedEvidenceFacets
  readonly completeness: EvidenceCompleteness
  readonly diagnostics: readonly EvidenceDiagnostic[]
}

/** Alias used by callers that want the composed facade rather than its snapshot name. */
export type SharedEvidence = SharedEvidenceSnapshot

export interface SharedEvidenceInput {
  readonly operationId: string
  readonly scope?: Omit<EvidenceScope, "operationId">
  readonly asOf?: EvidenceCursor
  readonly verification: VerifiableEvidence
  readonly refs?: readonly EvidenceRef[]
  readonly completeness?: EvidenceCompleteness
  readonly diagnostics?: readonly EvidenceDiagnostic[]
}

const DOMAINS: readonly EvidenceDomain[] = [
  "journal_record", "session_event", "checkpoint", "context_binding", "provider_attempt",
  "prompt_measurement", "wire_replay", "knowledge_entry", "memory_record", "memory_candidate",
]
const AUTHORITIES: readonly EvidenceAuthority[] = ["kernel_fact", "host_observation", "derived_projection"]
const AVAILABILITIES: readonly EvidenceAvailability[] = ["resident", "external", "missing", "redacted"]
const SETTLEMENTS: readonly MemorySettlement[] = ["pending", "accepted", "rejected"]
const RELATIONS: readonly EvidenceRelation[] = ["recalled_from", "settled_as"]
const MEMORY_DOMAINS: ReadonlySet<EvidenceDomain> = new Set(["memory_record", "memory_candidate"])
const SCOPE_FIELDS = ["operationId", "sessionId", "agentId", "tenantId", "namespace"] as const

type ScopeField = typeof SCOPE_FIELDS[number]

function requireText(value: string, label: string): string {
  if (typeof value !== "string" || value.length === 0) throw new TypeError(`${label} must be non-empty`)
  return value
}

function assertMember<T extends string>(value: T, allowed: readonly T[], label: string): void {
  if (!allowed.includes(value)) throw new TypeError(`${label} is invalid: ${value}`)
}

function copyScope(scope: EvidenceScope): EvidenceScope {
  return Object.freeze({
    operationId: requireText(scope.operationId, "scope.operationId"),
    ...(scope.sessionId === undefined ? {} : { sessionId: requireText(scope.sessionId, "scope.sessionId") }),
    ...(scope.agentId === undefined ? {} : { agentId: requireText(scope.agentId, "scope.agentId") }),
    ...(scope.tenantId === undefined ? {} : { tenantId: requireText(scope.tenantId, "scope.tenantId") }),
    ...(scope.namespace === undefined ? {} : { namespace: requireText(scope.namespace, "scope.namespace") }),
  })
}

function sameScope(left: EvidenceScope, right: EvidenceScope): boolean {
  return SCOPE_FIELDS.every((field: ScopeField) => left[field] === right[field])
}

function copyVerification(evidence: VerifiableEvidence): VerifiableEvidence {
  return Object.freeze({
    journal: Object.freeze(evidence.journal.map(bytes => new Uint8Array(bytes))),
    ...(evidence.sessionLogs === undefined ? {} : {
      sessionLogs: Object.freeze(evidence.sessionLogs.map(stream => Object.freeze(stream.map(bytes => new Uint8Array(bytes))))),
    }),
    ...(evidence.checkpoints === undefined ? {} : {
      checkpoints: Object.freeze(evidence.checkpoints.map(bytes => new Uint8Array(bytes))),
    }),
  })
}

function copyRef(ref: EvidenceRef, expectedScope?: EvidenceScope): EvidenceRef {
  if (!ref || typeof ref !== "object") throw new TypeError("evidence ref must be an object")
  requireText(ref.refId, "evidence ref.refId")
  assertMember(ref.domain, DOMAINS, "evidence ref.domain")
  assertMember(ref.authority, AUTHORITIES, "evidence ref.authority")
  assertMember(ref.availability, AVAILABILITIES, "evidence ref.availability")
  const scope = copyScope(ref.scope)
  if (expectedScope !== undefined && !sameScope(scope, expectedScope)) {
    throw new Error(`evidence ref ${ref.refId} crosses the shared evidence scope`)
  }
  if (ref.contentDigest !== undefined) requireText(ref.contentDigest, "evidence ref.contentDigest")
  if (ref.payloadRef !== undefined) requireText(ref.payloadRef, "evidence ref.payloadRef")
  if (ref.sizeBytes !== undefined && (!Number.isSafeInteger(ref.sizeBytes) || ref.sizeBytes < 0)) {
    throw new TypeError("evidence ref.sizeBytes must be a non-negative safe integer")
  }
  if (ref.version !== undefined) requireText(ref.version, "evidence ref.version")
  if (ref.provenance !== undefined) requireText(ref.provenance, "evidence ref.provenance")
  if (ref.settlement !== undefined) {
    assertMember(ref.settlement, SETTLEMENTS, "evidence ref.settlement")
    if (!MEMORY_DOMAINS.has(ref.domain)) throw new TypeError(`evidence ref.settlement is only valid on memory refs: ${ref.domain}`)
  }
  const retention = ref.retention === undefined ? undefined : Object.freeze({
    ...(ref.retention.pinned === undefined ? {} : { pinned: Boolean(ref.retention.pinned) }),
    ...(ref.retention.observedTtlDays === undefined ? {} : { observedTtlDays: ref.retention.observedTtlDays }),
    ...(ref.retention.prunable === undefined ? {} : { prunable: Boolean(ref.retention.prunable) }),
  })
  if (retention?.observedTtlDays !== undefined
    && (!Number.isSafeInteger(retention.observedTtlDays) || retention.observedTtlDays < 0)) {
    throw new TypeError("evidence ref.retention.observedTtlDays must be a non-negative safe integer")
  }
  const evidenceRefs = ref.evidenceRefs === undefined ? undefined : Object.freeze(
    ref.evidenceRefs.map(id => requireText(id, "evidence ref.evidenceRefs[]")))
  const links = ref.links === undefined ? undefined : copyLinks(ref.links)
  return Object.freeze({
    refId: ref.refId,
    domain: ref.domain,
    authority: ref.authority,
    scope,
    ...(ref.contentDigest === undefined ? {} : { contentDigest: ref.contentDigest }),
    ...(ref.payloadRef === undefined ? {} : { payloadRef: ref.payloadRef }),
    ...(ref.sizeBytes === undefined ? {} : { sizeBytes: ref.sizeBytes }),
    ...(ref.version === undefined ? {} : { version: ref.version }),
    availability: ref.availability,
    ...(ref.provenance === undefined ? {} : { provenance: ref.provenance }),
    ...(retention === undefined ? {} : { retention }),
    ...(ref.settlement === undefined ? {} : { settlement: ref.settlement }),
    ...(evidenceRefs === undefined ? {} : { evidenceRefs }),
    ...(links === undefined || links.length === 0 ? {} : { links }),
  })
}

function linkKey(link: EvidenceLink): string {
  return JSON.stringify([link.domain, link.refId, link.relation])
}

/** Validate, de-duplicate and order links so equal ref content serializes identically. */
function copyLinks(links: readonly EvidenceLink[]): readonly EvidenceLink[] {
  const byKey = new Map<string, EvidenceLink>()
  for (const link of links) {
    assertMember(link.relation, RELATIONS, "evidence link.relation")
    assertMember(link.domain, DOMAINS, "evidence link.domain")
    requireText(link.refId, "evidence link.refId")
    const copy = Object.freeze({ relation: link.relation, domain: link.domain, refId: link.refId })
    byKey.set(linkKey(copy), copy)
  }
  return Object.freeze([...byKey.entries()]
    .sort(([left], [right]) => compareOrdinal(left, right))
    .map(([, link]) => link))
}

function copyCursor(cursor: EvidenceCursor): EvidenceCursor {
  const position = (value: number | undefined, label: string, min: number) => {
    if (value !== undefined && (!Number.isSafeInteger(value) || value < min)) {
      throw new TypeError(`asOf.${label} must be a safe integer >= ${min}`)
    }
    return value === undefined ? {} : { [label]: value }
  }
  return Object.freeze({
    ...position(cursor.sessionLogSeq, "sessionLogSeq", -1),
    ...position(cursor.journalStepSeq, "journalStepSeq", -1),
    ...position(cursor.checkpointOrdinal, "checkpointOrdinal", 0),
  })
}

const COMPLETENESS_RANK: Record<EvidenceCompleteness, number> = { complete: 0, partial: 1, unavailable: 2 }

function worse(left: EvidenceCompleteness, right: EvidenceCompleteness): EvidenceCompleteness {
  return COMPLETENESS_RANK[right] > COMPLETENESS_RANK[left] ? right : left
}

function copyDiagnostic(diagnostic: EvidenceDiagnostic): EvidenceDiagnostic {
  requireText(diagnostic.code, "diagnostic.code")
  requireText(diagnostic.message, "diagnostic.message")
  if (diagnostic.domain !== undefined) assertMember(diagnostic.domain, DOMAINS, "diagnostic.domain")
  return Object.freeze({
    code: diagnostic.code,
    message: diagnostic.message,
    ...(diagnostic.domain === undefined ? {} : { domain: diagnostic.domain }),
    ...(diagnostic.refId === undefined ? {} : { refId: diagnostic.refId }),
  })
}

/** Evidence identity is domain-qualified: equal refIds in different domains are distinct refs. */
function refKey(domain: EvidenceDomain, refId: string): string {
  return JSON.stringify([domain, refId])
}

/** Ordinal (UTF-16 code unit) comparison; independent of the host locale. */
function compareOrdinal(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0
}

function sortRefs(refs: readonly EvidenceRef[]): readonly EvidenceRef[] {
  return Object.freeze([...refs].sort((left, right) =>
    compareOrdinal(refKey(left.domain, left.refId), refKey(right.domain, right.refId))))
}

/** Completeness the refs themselves prove, independent of what the caller declared. */
function deriveCompleteness(
  refs: readonly EvidenceRef[],
  verification: VerifiableEvidence,
): { completeness: EvidenceCompleteness, diagnostics: EvidenceDiagnostic[] } {
  const diagnostics: EvidenceDiagnostic[] = []
  let completeness: EvidenceCompleteness = "complete"
  if (refs.length === 0 && verification.journal.length === 0) {
    diagnostics.push({ code: "evidence_empty", message: "snapshot has no references and no journal bytes" })
    return { completeness: "unavailable", diagnostics }
  }
  const present = new Set(refs.map(ref => refKey(ref.domain, ref.refId)))
  for (const ref of refs) {
    for (const link of ref.links ?? []) {
      if (present.has(refKey(link.domain, link.refId))) continue
      completeness = worse(completeness, "partial")
      diagnostics.push({
        code: "evidence_link_unresolved",
        message: `${link.relation} target ${link.domain}/${link.refId} is not in the snapshot`,
        domain: ref.domain,
        refId: ref.refId,
      })
    }
    // Knowledge is complete only when both its identity (digest) and its origin (provenance) are known.
    if (ref.domain === "knowledge_entry" && ref.contentDigest === undefined) {
      completeness = worse(completeness, "partial")
      diagnostics.push({
        code: "knowledge_digest_missing",
        message: "knowledge reference has no content digest",
        domain: ref.domain,
        refId: ref.refId,
      })
    }
    if (ref.domain === "knowledge_entry" && ref.provenance === undefined) {
      completeness = worse(completeness, "partial")
      diagnostics.push({
        code: "knowledge_provenance_missing",
        message: "knowledge reference has no provenance from its sidecar",
        domain: ref.domain,
        refId: ref.refId,
      })
    }
    if (ref.availability === "missing") {
      completeness = worse(completeness, "partial")
      diagnostics.push({
        code: "evidence_missing",
        message: "referenced evidence is not available from its source",
        domain: ref.domain,
        refId: ref.refId,
      })
    }
  }
  return { completeness, diagnostics }
}

function splitFacets(refs: readonly EvidenceRef[]): SharedEvidenceFacets {
  const executionDomains = new Set<EvidenceDomain>(["provider_attempt", "prompt_measurement", "wire_replay", "context_binding"])
  const execution = refs.filter(ref => executionDomains.has(ref.domain))
  const references = refs.filter(ref => !executionDomains.has(ref.domain))
  return Object.freeze({
    ...(execution.length === 0 ? {} : { execution: Object.freeze(execution) }),
    ...(references.length === 0 ? {} : { references: Object.freeze(references) }),
  })
}

export class SharedEvidenceBuilder {
  private readonly identity: SharedEvidenceIdentity
  private readonly asOf: EvidenceCursor | undefined
  private readonly verification: VerifiableEvidence
  private readonly refsByKey = new Map<string, EvidenceRef>()
  private readonly declaredDiagnostics: EvidenceDiagnostic[]
  private declaredCompleteness: EvidenceCompleteness

  constructor(input: SharedEvidenceInput) {
    const operationId = requireText(input.operationId, "operationId")
    const scope = copyScope({ operationId, ...input.scope })
    this.identity = Object.freeze({ operationId, scope })
    this.asOf = input.asOf === undefined ? undefined : copyCursor(input.asOf)
    this.verification = copyVerification(input.verification)
    this.declaredDiagnostics = (input.diagnostics ?? []).map(copyDiagnostic)
    this.declaredCompleteness = input.completeness ?? "complete"
    assertMember(this.declaredCompleteness, ["complete", "partial", "unavailable"], "completeness")
    for (const ref of input.refs ?? []) this.addRef(ref)
  }

  addRef(input: EvidenceRef): this {
    const ref = copyRef(input, this.identity.scope)
    const key = refKey(ref.domain, ref.refId)
    const existing = this.refsByKey.get(key)
    if (existing !== undefined) {
      if (JSON.stringify(existing) !== JSON.stringify(ref)) {
        throw new Error(`conflicting evidence ref: ${ref.domain}/${ref.refId}`)
      }
      this.declaredDiagnostics.push(copyDiagnostic({
        code: "duplicate_evidence_ref",
        message: "identical evidence ref was supplied more than once",
        domain: ref.domain,
        refId: ref.refId,
      }))
      return this
    }
    this.refsByKey.set(key, ref)
    return this
  }

  markPartial(diagnostic: EvidenceDiagnostic): this {
    this.declaredCompleteness = worse(this.declaredCompleteness, "partial")
    this.declaredDiagnostics.push(copyDiagnostic(diagnostic))
    return this
  }

  build(): SharedEvidenceSnapshot {
    const refs = sortRefs([...this.refsByKey.values()])
    const verification = copyVerification(this.verification)
    const derived = deriveCompleteness(refs, verification)
    const snapshot = {
      identity: this.identity,
      ...(this.asOf === undefined ? {} : { asOf: this.asOf }),
      refs,
      facets: splitFacets(refs),
      completeness: worse(this.declaredCompleteness, derived.completeness),
      diagnostics: Object.freeze([...this.declaredDiagnostics, ...derived.diagnostics.map(copyDiagnostic)]),
    }
    Object.defineProperty(snapshot, "verification", { enumerable: true, get: () => copyVerification(verification) })
    return Object.freeze(snapshot) as SharedEvidenceSnapshot
  }
}

/** Build a deterministic, immutable host-side evidence snapshot without performing I/O. */
export function createSharedEvidence(input: SharedEvidenceInput): SharedEvidenceSnapshot {
  return new SharedEvidenceBuilder(input).build()
}

/** Read-only query facade over one immutable snapshot. */
export class SessionEvidenceReader {
  readonly snapshot: SharedEvidenceSnapshot

  constructor(snapshot: SharedEvidenceSnapshot) {
    this.snapshot = snapshot
  }

  /** Read the operation's logs and wrap the resulting snapshot. */
  static async read(sources: SessionEvidenceSources): Promise<SessionEvidenceReader> {
    return new SessionEvidenceReader(await readSessionEvidence(sources))
  }

  refs(domain?: EvidenceDomain): readonly EvidenceRef[] {
    return domain === undefined ? this.snapshot.refs : this.snapshot.refs.filter(ref => ref.domain === domain)
  }

  byRefId(domain: EvidenceDomain, refId: string): EvidenceRef | undefined {
    return this.snapshot.refs.find(ref => ref.domain === domain && ref.refId === refId)
  }

  byDigest(contentDigest: string): readonly EvidenceRef[] {
    return this.snapshot.refs.filter(ref => ref.contentDigest === contentDigest)
  }

  /** The refs `ref` links to (optionally one relation), in link order; unresolved links are skipped. */
  linked(ref: EvidenceRef, relation?: EvidenceRelation): readonly EvidenceRef[] {
    return (ref.links ?? [])
      .filter(link => relation === undefined || link.relation === relation)
      .map(link => this.byRefId(link.domain, link.refId))
      .filter((target): target is EvidenceRef => target !== undefined)
  }

  /** The refs that link to `ref` (optionally one relation): e.g. the knowledge copies of a memory. */
  linking(ref: EvidenceRef, relation?: EvidenceRelation): readonly EvidenceRef[] {
    return this.snapshot.refs.filter(source => (source.links ?? []).some(link =>
      link.domain === ref.domain && link.refId === ref.refId && (relation === undefined || link.relation === relation)))
  }
}

/**
 * Read-only host lookup for facts the logs do not carry. Every method is optional; an absent or
 * failing lookup degrades the snapshot to partial instead of inventing the fact.
 */
export interface EvidenceSidecar {
  knowledgeProvenance?(entry: { readonly entryId: string, readonly contentDigest: string }):
    string | undefined | Promise<string | undefined>
  /**
   * The candidate store's current receipt for a candidate seen in the log. This is a lookup, never
   * `MemoryCandidateSink.submit`: resubmitting would be a write. An accepted receipt refines the
   * candidate ref with the sink's `digest` and uses its opaque `record_ref` as the payload locator.
   */
  memoryCandidateReceipt?(candidate: { readonly agentId: string, readonly candidateId: string }):
    MemoryCandidateReceipt | undefined | Promise<MemoryCandidateReceipt | undefined>
}

export interface SessionEvidenceSources {
  readonly operationId: string
  /** The SessionLog stream of this operation. */
  readonly sessionId: string
  readonly scope?: Omit<EvidenceScope, "operationId" | "sessionId">
  readonly sessionLog: SessionLog
  /** Absent means the journal plane was not provided; the snapshot is then partial. */
  readonly kernelJournal?: KernelJournal
  readonly sidecar?: EvidenceSidecar
}

const MAX_DIAGNOSTIC_MESSAGE = 200
const utf8 = new TextEncoder()

function describeError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error)
  return text.length > MAX_DIAGNOSTIC_MESSAGE ? `${text.slice(0, MAX_DIAGNOSTIC_MESSAGE)}…` : text
}

/** Accumulates refs read from logs; a later observation of the same ref refines the earlier one. */
class ObservedRefs {
  private readonly byKey = new Map<string, EvidenceRef>()

  constructor(private readonly scope: EvidenceScope) {}

  put(ref: Omit<EvidenceRef, "scope">): void {
    const key = refKey(ref.domain, ref.refId)
    const defined = Object.fromEntries(Object.entries(ref).filter(([, value]) => value !== undefined))
    this.byKey.set(key, { ...this.byKey.get(key), ...defined, scope: this.scope } as EvidenceRef)
  }

  /** Update fields of a ref already observed in the logs; a sidecar never introduces a ref. */
  refine(domain: EvidenceDomain, refId: string, fields: Partial<Omit<EvidenceRef, "refId" | "domain" | "scope">>): void {
    const existing = this.byKey.get(refKey(domain, refId))
    if (existing !== undefined) this.put({ ...existing, ...fields })
  }

  has(domain: EvidenceDomain, refId: string): boolean {
    return this.byKey.has(refKey(domain, refId))
  }

  values(): EvidenceRef[] {
    return [...this.byKey.values()]
  }
}

function memoryInScope(memory: MemoryScope, scope: EvidenceScope): boolean {
  return (scope.tenantId === undefined || scope.tenantId === memory.tenant_id)
    && (scope.namespace === undefined || scope.namespace === memory.namespace)
}

/**
 * The bytes a session event is evidenced by: canonical JSON. Keys are sorted by code unit, there is
 * no whitespace, and object members that are absent or `null` are dropped (an absent optional field
 * is never `null` in the shared wire, but some SDKs still write one). Integral numbers have no
 * fraction (JSON.stringify already writes `1` for `1.0`). The four SDKs serialize events in
 * different key orders, so only this form gives one digest per event across SDKs.
 */
export function canonicalEventJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(item => item === undefined ? "null" : canonicalEventJson(item)).join(",")}]`
  const object = value as Record<string, unknown>
  return `{${Object.keys(object).filter(key => object[key] !== undefined && object[key] !== null).sort(compareOrdinal)
    .map(key => `${JSON.stringify(key)}:${canonicalEventJson(object[key])}`).join(",")}}`
}

function sha256(bytes: Uint8Array): string {
  return sha256Hex(new TextDecoder().decode(bytes))
}

/** Knowledge-key prefixes the SDK runners mint, and the origin each one names. */
const KNOWLEDGE_ORIGINS: Readonly<Record<string, string>> = { memory: "memory", skill: "skill" }

export interface KnowledgeIdentity {
  readonly refId: string
  readonly key: string
  readonly origin?: string
  readonly links?: readonly EvidenceLink[]
}

/**
 * A knowledge entry's evidence identity is its key and content (`<key>@<digest>`). The kernel's
 * `entry_id` is `knowledge:<ordinal>:<key>:<digest>`; the ordinal is a position that shifts when an
 * earlier entry is swept, so it is not part of the identity. An `entry_id` in any other shape is
 * used whole as the key. Keys the runners mint name the entry's origin: `memory:<record_id>` is a
 * recalled record (linked to it), `skill:<name>` a skill body.
 */
export function knowledgeIdentity(entryId: string, contentDigest: string): KnowledgeIdentity {
  const prefix = "knowledge:"
  const suffix = `:${contentDigest}`
  let key = entryId
  if (entryId.startsWith(prefix) && entryId.endsWith(suffix)) {
    const body = entryId.slice(prefix.length, entryId.length - suffix.length)
    const separator = body.indexOf(":")
    if (separator > 0 && /^\d+$/.test(body.slice(0, separator)) && separator < body.length - 1) {
      key = body.slice(separator + 1)
    }
  }
  const refId = `${key}@${contentDigest}`
  const separator = key.indexOf(":")
  const origin = separator > 0 ? KNOWLEDGE_ORIGINS[key.slice(0, separator)] : undefined
  if (origin === undefined) return { refId, key }
  const target = key.slice(separator + 1)
  if (origin === "memory" && target.length > 0) {
    return { refId, key, origin, links: [{ relation: "recalled_from", domain: "memory_record", refId: target }] }
  }
  return { refId, key, origin }
}

/** Lower one SessionLog event onto evidence refs. Events with no evidence meaning are skipped. */
function observeEvent(
  seq: number,
  event: SessionEvent,
  bytes: Uint8Array,
  refs: ObservedRefs,
  scope: EvidenceScope,
  knowledge: Map<string, { entryId: string, contentDigest: string }>,
  diagnostics: EvidenceDiagnostic[],
): void {
  const payloadRef = `sessionlog:${seq}`
  // Every event is evidence of itself; the digest covers exactly the bytes handed to verification.
  refs.put({
    refId: String(seq),
    domain: "session_event",
    authority: "host_observation",
    contentDigest: sha256(bytes),
    sizeBytes: bytes.byteLength,
    payloadRef,
    availability: "resident",
  })
  const outOfScope = (domain: EvidenceDomain, refId: string) => diagnostics.push({
    code: "memory_scope_mismatch",
    message: "memory event belongs to another tenant or namespace and was excluded",
    domain,
    refId,
  })
  switch (event.kind) {
    case "provider_attempt":
      refs.put({
        refId: `${event.effect_id}#${event.attempt_seq}`,
        domain: "provider_attempt",
        authority: "host_observation",
        payloadRef,
        availability: "resident",
      })
      return
    case "prompt_measured":
      refs.put({
        refId: event.effect_id ?? `turn:${event.turn}`,
        domain: "prompt_measurement",
        authority: "host_observation",
        payloadRef,
        availability: "resident",
        provenance: event.measurement.source.kind,
      })
      return
    case "llm_completed":
      if (event.wire_evidence === undefined) return
      refs.put({
        refId: event.invocation_id ?? event.effect_id ?? `turn:${event.turn}`,
        domain: "wire_replay",
        authority: "host_observation",
        payloadRef,
        availability: "resident",
        ...(event.wire_evidence.response_id === undefined ? {} : { version: event.wire_evidence.response_id }),
      })
      return
    case "context_prepared":
      refs.put({
        refId: event.effect_id,
        domain: "context_binding",
        authority: "derived_projection",
        contentDigest: event.preparation.binding.digest,
        payloadRef,
        availability: "resident",
      })
      for (const entry of event.preparation.state.knowledge) {
        const identity = knowledgeIdentity(entry.entry_id, entry.content_digest)
        knowledge.set(identity.refId, { entryId: entry.entry_id, contentDigest: entry.content_digest })
        refs.put({
          refId: identity.refId,
          domain: "knowledge_entry",
          authority: "derived_projection",
          contentDigest: entry.content_digest,
          payloadRef,
          availability: "resident",
          provenance: identity.origin,
          links: identity.links,
        })
      }
      return
    case "memory_written":
      if (!memoryInScope(event.scope, scope)) return void outOfScope("memory_record", event.record_id)
      refs.put({
        refId: event.record_id,
        domain: "memory_record",
        authority: "host_observation",
        payloadRef,
        sizeBytes: event.size_bytes,
        availability: "external",
        settlement: "accepted",
      })
      return
    case "memory_retrieval_result":
      for (const hit of event.hits) {
        const record = hit.record
        if (!memoryInScope(record.scope, scope)) {
          outOfScope("memory_record", record.record_id)
          continue
        }
        refs.put({
          refId: record.record_id,
          domain: "memory_record",
          authority: "host_observation",
          payloadRef,
          availability: "external",
          provenance: `${record.provenance.author}/${record.provenance.trust}`,
          settlement: "accepted",
          // Empty lists are omitted, as the Rust SessionLog never writes them.
          evidenceRefs: record.provenance.evidence_refs?.length ? record.provenance.evidence_refs : undefined,
          retention: {
            pinned: record.pinned,
            ...(record.ttl_days === undefined ? {} : { observedTtlDays: record.ttl_days }),
          },
        })
      }
      return
    case "memory_candidate_pending":
    case "memory_candidate_rejected":
      refs.put({
        refId: event.candidate_id,
        domain: "memory_candidate",
        authority: "host_observation",
        payloadRef,
        availability: "external",
        provenance: event.source,
        settlement: event.kind === "memory_candidate_pending" ? "pending" : "rejected",
        version: event.receipt_id,
      })
      return
    default:
      return
  }
}

/**
 * Refine logged candidates with the sink's current receipt. The log stays authoritative for what
 * it recorded: a rejection is final, so a receipt that contradicts it is a conflict, not an update.
 */
async function refineCandidates(
  sidecar: EvidenceSidecar | undefined,
  scope: EvidenceScope,
  refs: ObservedRefs,
  diagnostics: EvidenceDiagnostic[],
): Promise<void> {
  const lookup = sidecar?.memoryCandidateReceipt
  const candidates = refs.values().filter(ref => ref.domain === "memory_candidate")
  if (lookup === undefined || candidates.length === 0) return
  const agentId = scope.agentId
  if (agentId === undefined) {
    diagnostics.push({
      code: "memory_candidate_agent_unscoped",
      message: "candidate receipts are keyed by agentId, which the evidence scope does not carry",
      domain: "memory_candidate",
    })
    return
  }
  for (const candidate of candidates) {
    const refId = candidate.refId
    let receipt: MemoryCandidateReceipt | undefined
    try {
      receipt = await lookup.call(sidecar, { agentId, candidateId: refId })
    } catch (error) {
      diagnostics.push({ code: "sidecar_unreadable", message: describeError(error), domain: "memory_candidate", refId })
      continue
    }
    if (receipt === undefined) continue
    const conflict = receiptConflict(candidate, receipt)
    if (conflict !== undefined) {
      diagnostics.push({ code: "memory_candidate_receipt_conflict", message: conflict, domain: "memory_candidate", refId })
      continue
    }
    // A record written in this log under the receipt's record_ref is the candidate's settled form.
    const accepted = receipt.status === "accepted" ? receipt : undefined
    const settled = accepted !== undefined && refs.has("memory_record", accepted.record_ref)
    refs.refine("memory_candidate", refId, {
      settlement: receipt.status,
      version: receipt.receipt_id,
      ...(accepted === undefined ? {} : { contentDigest: accepted.digest, payloadRef: accepted.record_ref }),
      ...(settled ? { links: [{ relation: "settled_as", domain: "memory_record", refId: accepted.record_ref }] } : {}),
    })
  }
}

/**
 * `receipt_id` names one answer of the sink (spec §2.3): a later review may answer again under a
 * new id (pending → accepted), but the same status under a different id contradicts the log, and
 * a logged rejection is final.
 */
function receiptConflict(candidate: EvidenceRef, receipt: MemoryCandidateReceipt): string | undefined {
  if (receipt.candidate_id !== candidate.refId) {
    return `sink receipt names candidate ${receipt.candidate_id}`
  }
  if (candidate.settlement === "rejected" && receipt.status !== "rejected") {
    return `sink receipt (${receipt.status}) disagrees with the logged candidate (rejected)`
  }
  if (receipt.status === candidate.settlement && candidate.version !== undefined && receipt.receipt_id !== candidate.version) {
    return `sink receipt ${receipt.receipt_id} renames the logged ${receipt.status} answer ${candidate.version}`
  }
  return undefined
}

/**
 * Read one operation's evidence from the existing logs into an immutable snapshot. Only read
 * methods are called (`SessionLog.read`, `KernelJournal.readFrom`/`latestCheckpoint`, sidecar
 * lookups); a source that fails to read becomes a diagnostic and degrades completeness.
 */
export async function readSessionEvidence(sources: SessionEvidenceSources): Promise<SharedEvidenceSnapshot> {
  const operationId = requireText(sources.operationId, "operationId")
  const sessionId = requireText(sources.sessionId, "sessionId")
  const scope = copyScope({ ...sources.scope, operationId, sessionId })
  const refs = new ObservedRefs(scope)
  const diagnostics: EvidenceDiagnostic[] = []
  const knowledge = new Map<string, { entryId: string, contentDigest: string }>()
  const unreadable = (code: string, error: unknown) => diagnostics.push({ code, message: describeError(error) })

  let sessionSeq: number | undefined
  const sessionBlobs: Uint8Array[] = []
  try {
    let lastSeq = -1
    for (const { seq, event } of await sources.sessionLog.read(sessionId)) {
      lastSeq = Math.max(lastSeq, seq)
      const bytes = utf8.encode(canonicalEventJson(event))
      sessionBlobs.push(bytes)
      observeEvent(seq, event, bytes, refs, scope, knowledge, diagnostics)
    }
    sessionSeq = lastSeq
  } catch (error) {
    unreadable("session_log_unreadable", error)
  }

  let journalStep: number | undefined
  let checkpointOrdinal: number | undefined
  const journal: Uint8Array[] = []
  const checkpoints: Uint8Array[] = []
  if (sources.kernelJournal === undefined) {
    diagnostics.push({ code: "journal_not_provided", message: "no KernelJournal was supplied" })
  } else {
    try {
      const entries = await sources.kernelJournal.readFrom(operationId)
      let lastStep = -1
      for (const entry of entries) {
        lastStep = entry.step_seq
        journal.push(entry.record_bytes)
        refs.put({
          refId: String(entry.step_seq),
          domain: "journal_record",
          authority: "kernel_fact",
          contentDigest: entry.record_digest,
          sizeBytes: entry.record_bytes.byteLength,
          availability: "resident",
        })
      }
      const checkpoint = await sources.kernelJournal.latestCheckpoint(operationId)
      journalStep = lastStep
      if (checkpoint !== undefined) {
        checkpointOrdinal = checkpoint.ordinal
        checkpoints.push(checkpoint.checkpoint_bytes)
        refs.put({
          refId: checkpoint.checkpoint_id,
          domain: "checkpoint",
          authority: "kernel_fact",
          contentDigest: checkpoint.state_digest,
          sizeBytes: checkpoint.checkpoint_bytes.byteLength,
          version: String(checkpoint.ordinal),
          availability: "resident",
        })
      }
      // A pruned prefix is only recoverable through a checkpoint that reaches the first retained record.
      const firstStep = entries[0]?.step_seq ?? 0
      if (firstStep > 0 && (checkpoint === undefined || checkpoint.through_step_seq < firstStep - 1)) {
        diagnostics.push({
          code: "journal_prefix_pruned",
          message: `journal records before step ${firstStep} are pruned and no checkpoint covers them`,
          domain: "journal_record",
        })
      }
    } catch (error) {
      unreadable("journal_unreadable", error)
    }
  }

  const lookup = sources.sidecar?.knowledgeProvenance
  if (lookup !== undefined) {
    for (const [refId, entry] of knowledge) {
      try {
        const provenance = await lookup.call(sources.sidecar, entry)
        if (provenance !== undefined) refs.refine("knowledge_entry", refId, { provenance })
      } catch (error) {
        diagnostics.push({ code: "sidecar_unreadable", message: describeError(error), domain: "knowledge_entry", refId })
      }
    }
  }

  await refineCandidates(sources.sidecar, scope, refs, diagnostics)

  const asOf: EvidenceCursor = {
    ...(sessionSeq === undefined ? {} : { sessionLogSeq: sessionSeq }),
    ...(journalStep === undefined ? {} : { journalStepSeq: journalStep }),
    ...(checkpointOrdinal === undefined ? {} : { checkpointOrdinal }),
  }
  const builder = new SharedEvidenceBuilder({
    operationId,
    scope: { ...sources.scope, sessionId },
    ...(Object.keys(asOf).length === 0 ? {} : { asOf }),
    verification: {
      journal,
      ...(sessionBlobs.length === 0 ? {} : { sessionLogs: [sessionBlobs] }),
      ...(checkpoints.length === 0 ? {} : { checkpoints }),
    },
    refs: refs.values(),
  })
  for (const diagnostic of diagnostics) builder.markPartial(diagnostic)
  return builder.build()
}
