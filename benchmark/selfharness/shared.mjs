/**
 * Self-Harness shared layer (V2-S4) — DESIGN-FINAL, IMPLEMENTATION DEFERRED.
 *
 * v2 ships the two-layer lineage's TYPES and its promotion GATE only, not a production aggregation
 * pipeline. Per the spec, production shared aggregation lands with the per-model-profile corpus work
 * (the same "deferred until there is task-language scale" item v1 carried): adjudicating shared
 * promotions needs a real multi-tenant corpus, not the loop machinery. This module is the seam.
 *
 * The lineage has two layers:
 *   - the SCOPE layer (V2-S1) — one tenant's harness, evolving automatically inside its scope; and
 *   - the SHARED layer — an edit promoted to ALL scopes, allowed only under three conditions:
 *       (1) the same signature cluster recurs across ≥2 INDEPENDENT scopes,
 *       (2) the aggregate that justifies it carries NO scope's verbatim transcript (only signatures +
 *           counts — otherwise one tenant's data would be written into a global instruction), and
 *       (3) the promotion is HUMAN — there is no auto path for a cross-tenant edit.
 *
 * `aggregateSharedEvidence` builds the privacy-safe aggregate by CONSTRUCTION: it merges already-built
 * per-scope `EvidenceBundle`s at the CLUSTER level (signature + size + per-scope counts), copying no
 * excerpt text and no taskIds forward. It deliberately does NOT route multi-scope records through
 * `buildEvidenceBundle` — that function's mixed-scope guard throws by design (the V2-S1 seam), and the
 * whole point here is a legitimate cross-scope roll-up that never touches raw records.
 *
 * @typedef {import("./evidence.mjs").EvidenceBundle} EvidenceBundle
 * @typedef {import("./evidence.mjs").FailureSignature} FailureSignature
 * @typedef {import("../../node/src/harness/manifest.js").HarnessPatch} HarnessPatch
 *
 * @typedef {Object} SharedCluster
 * @property {FailureSignature} signature   Machine-fact signature (cause + symptom) — no transcript text.
 * @property {number} totalSize             Sum of the cluster's size across every contributing scope.
 * @property {Array<{ scope: string, size: number }>} scopes   Per-scope counts, scope-name sorted.
 *
 * @typedef {Object} SharedEvidence
 * @property {SharedCluster[]} clusters      Signature-keyed, name-sorted; contains NO excerpt / taskId.
 *
 * @typedef {Object} SharedProposal
 * @property {HarnessPatch} patch
 * @property {string} tier                   Promotion tier of the patch's surface (recorded, any tier).
 * @property {string[]} scopes               The distinct scopes the signature recurred in (sorted).
 * @property {string} approvedBy             The human approval token that authorized the shared promotion.
 */

import { loadSdk } from "../utils/sdk.mjs"

let _sdk
async function sdk() {
  return (_sdk ??= await loadSdk())
}

/** Reconstruct a cluster's exact-match key from its signature (same encoding as evidence clustering). */
function signatureKey(signature) {
  return JSON.stringify(signature)
}

/**
 * Roll per-scope `EvidenceBundle`s up into a signature-only shared aggregate. The result carries the
 * machine-fact signature, the summed size, and per-scope counts — and, BY CONSTRUCTION, nothing else:
 * no excerpt text and no taskIds cross the aggregation boundary (cross-tenant privacy).
 * @param {EvidenceBundle[]} bundles   One per scope (≥1).
 * @returns {SharedEvidence}
 */
export function aggregateSharedEvidence(bundles) {
  if (!Array.isArray(bundles) || bundles.length === 0) {
    throw new Error("aggregateSharedEvidence: at least one EvidenceBundle is required")
  }
  /** @type {Map<string, { signature: FailureSignature, totalSize: number, scopes: Map<string, number> }>} */
  const byKey = new Map()
  for (const bundle of bundles) {
    const scope = bundle?.scope ?? "default"
    for (const cluster of bundle?.clusters ?? []) {
      const key = signatureKey(cluster.signature)
      let entry = byKey.get(key)
      if (!entry) {
        entry = { signature: cluster.signature, totalSize: 0, scopes: new Map() }
        byKey.set(key, entry)
      }
      entry.totalSize += cluster.size
      entry.scopes.set(scope, (entry.scopes.get(scope) ?? 0) + cluster.size)
    }
  }
  const clusters = [...byKey.entries()]
    .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)) // signature-key name sort → deterministic
    .map(([, entry]) => ({
      signature: entry.signature, // machine facts only — carries no transcript excerpt / taskId
      totalSize: entry.totalSize,
      scopes: [...entry.scopes.entries()]
        .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
        .map(([scope, size]) => ({ scope, size })),
    }))
  return { clusters }
}

/**
 * The shared-layer promotion GATE. Enforces the three conditions and throws on any violation; there is
 * NO default and NO auto path — a shared promotion is always human. Returns a frozen proposal record
 * suitable for persisting (no lineage writing / no loop wiring happens here — that is the deferred
 * production work).
 * @param {{ patch: HarnessPatch, aggregate: SharedEvidence, decision: { approvedBy: string } }} args
 * @returns {Promise<Readonly<SharedProposal>>}
 */
export async function promoteToShared({ patch, aggregate, decision }) {
  const { surfaceTier } = await sdk()

  // (2) Explicit human approval — always required, no default.
  const approvedBy = decision?.approvedBy
  if (typeof approvedBy !== "string" || approvedBy.length === 0) {
    throw new Error("promoteToShared: decision.approvedBy must be a non-empty human-approval token — shared promotion is always human")
  }

  // (3) The surface must map to a known tier (throws on unknown) — recorded on the proposal.
  const tier = surfaceTier(patch?.targetSurface)

  // (1) The patch's targetCluster signature must recur across ≥2 distinct scopes in the aggregate.
  const cluster = (aggregate?.clusters ?? []).find(c => signatureKey(c.signature) === patch?.targetCluster)
  if (!cluster) {
    throw new Error(`promoteToShared: patch.targetCluster "${patch?.targetCluster}" is absent from the shared aggregate`)
  }
  const distinctScopes = [...new Set((cluster.scopes ?? []).map(s => s.scope))].sort()
  if (distinctScopes.length < 2) {
    throw new Error(
      `promoteToShared: cluster "${patch?.targetCluster}" appears in only ${distinctScopes.length} scope(s); shared promotion requires ≥2 distinct scopes`,
    )
  }

  return Object.freeze({ patch, tier, scopes: distinctScopes, approvedBy })
}
