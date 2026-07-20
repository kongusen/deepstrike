/**
 * Self-Harness H1.1 + H1.3 — the harness face as DATA.
 *
 * A `HarnessManifest` is a versioned, hashable lineage node: the editable surfaces a fixed model may
 * rewrite about its OWN harness — instruction slots, nudge rules, and a whitelisted `RuntimeOptions`
 * subset — plus the audit trail binding each edit to the failure cluster it targets. Every function
 * here is pure and deterministic (no clock, no randomness, no I/O), so a manifest digest is a stable
 * identity across processes and the propose→validate→promote loop replays byte-for-byte.
 *
 * The whitelist is the safety boundary: governance / quota / reliability surfaces are deliberately
 * absent, so a proposer can never rewrite them (spec design principle: conservative promotion).
 */
import { createHash } from "node:crypto"
import type { MemoryPolicy } from "../kernel.js"
import type { RuntimeOptions } from "../runtime/runner.js"
import type { NudgeRule } from "./nudge.js"
import { validateNudgeRules } from "./nudge.js"

// ── Instruction slots ────────────────────────────────────────────────────────

export interface InstructionProfile {
  /** Start-up protocol (paper: build_bootstrap_instruction). */
  bootstrap?: string
  /** Execution protocol. */
  execution?: string
  /** Closing verification protocol. */
  verification?: string
  /** Failure-recovery protocol. */
  failureRecovery?: string
}

const INSTRUCTION_SLOTS = ["bootstrap", "execution", "verification", "failureRecovery"] as const
type InstructionSlot = (typeof INSTRUCTION_SLOTS)[number]

/** Per-slot upper bound enforced at load and on every `applyPatch` set. */
const MAX_INSTRUCTION_CHARS = 4000

/** A scope key becomes a directory segment; restrict it to a single path-safe token (no separators). */
const SCOPE_PATTERN = /^[A-Za-z0-9._-]{1,64}$/

/**
 * Compose the four instruction slots onto `base` in the fixed order base → bootstrap → execution →
 * verification → failureRecovery, joined with `"\n\n"`, skipping empty slots. All-empty ⇒ `base`
 * unchanged (identity — the zero-instructions run is byte-for-byte the pre-feature run). The order is
 * fixed and empty slots are dropped so the composed prefix is byte-stable (prefix-cache axiom).
 */
export function composeSystemPrompt(
  base: string | undefined,
  instructions?: InstructionProfile,
): string | undefined {
  const parts: string[] = []
  if (base) parts.push(base)
  for (const slot of INSTRUCTION_SLOTS) {
    const text = instructions?.[slot]
    if (text) parts.push(text)
  }
  return parts.length === 0 ? base : parts.join("\n\n")
}

// ── Runtime patch (the whitelisted RuntimeOptions subset) ─────────────────────

/**
 * The exact `RuntimeOptions` fields a manifest may drive. Derived via `Pick` so field names and types
 * track `RuntimeOptions` verbatim; anything outside this set is rejected by `applyManifest`/`applyPatch`.
 */
export type HarnessRuntimePatch = Pick<
  RuntimeOptions,
  | "maxTurns"
  | "maxTotalTokens"
  | "criteriaGate"
  | "repeatFuse"
  | "entropyWatch"
  | "knowledgeBudgetRatio"
  | "skillLeaseTurns"
> & Pick<MemoryPolicy, "retrievalTopK" | "promotionRecallThreshold">

const MEMORY_POLICY_PATCH_KEYS = ["retrievalTopK", "promotionRecallThreshold"] as const
type MemoryPolicyPatchKey = (typeof MEMORY_POLICY_PATCH_KEYS)[number]

const RUNTIME_PATCH_KEYS: readonly string[] = [
  "maxTurns",
  "maxTotalTokens",
  "criteriaGate",
  "repeatFuse",
  "entropyWatch",
  "knowledgeBudgetRatio",
  "skillLeaseTurns",
  ...MEMORY_POLICY_PATCH_KEYS,
]

// ── Manifest + patch shapes ──────────────────────────────────────────────────

export interface HarnessManifest {
  manifestVersion: 1
  /** Parent manifest digest; `null` for a seed. */
  parent: string | null
  /** Target-model identifier (per-model profile scenarios). */
  modelProfile?: string
  /**
   * Opaque isolation key — host decides its semantics (user / tenant / agent-group). Orthogonal to
   * `modelProfile` (never concatenate the two — that reprises the identity-scoping bug class); absent
   * ⇒ the host treats it as `"default"`. It rides canonical JSON, so digests domain-separate by scope,
   * but an absent scope leaves a v1-shaped manifest's digest byte-identical (canonicalJson skips
   * undefined). Becomes a lineage directory name downstream, hence the path-safe character bound.
   */
  scope?: string
  instructions?: InstructionProfile
  nudges?: NudgeRule[]
  runtime?: HarnessRuntimePatch
  /** The proposer's edit whitelist — patches may only target a surface listed here. */
  editableSurfaces: string[]
  audit?: {
    round: number
    createdBy: "seed" | "proposer"
    targetCluster?: string
    rationale?: string
    deltaHeldIn?: number
    deltaHeldOut?: number
  }
}

export interface HarnessPatch {
  /** Surface path — must be in the manifest's `editableSurfaces`. */
  targetSurface: string
  /** `append` applies only to nudges; `remove` clears a slot or drops a nudge by id. */
  op: "set" | "append" | "remove"
  value?: unknown
  rationale: string
  /** Failure-cluster key this edit is bound to (paper: one edit per failure mechanism). */
  targetCluster: string
  expectedEffect: string
}

// ── Canonical JSON + digest ──────────────────────────────────────────────────

/** Deterministic serialization: recursive key sort, undefined-valued keys skipped, arrays ordered. */
function canonicalJson(value: unknown): string {
  if (value === null) return "null"
  if (typeof value === "string" || typeof value === "boolean") return JSON.stringify(value)
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("harness manifest requires finite numbers")
    return JSON.stringify(value)
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`
  if (typeof value === "object") {
    const obj = value as Record<string, unknown>
    const keys = Object.keys(obj).filter(key => obj[key] !== undefined).sort()
    return `{${keys.map(key => `${JSON.stringify(key)}:${canonicalJson(obj[key])}`).join(",")}}`
  }
  throw new TypeError(`harness manifest holds a non-serializable value: ${typeof value}`)
}

/** sha-256 hex over the manifest's canonical JSON — the manifest's stable identity. */
export function manifestDigest(manifest: HarnessManifest): string {
  return createHash("sha256").update(canonicalJson(manifest), "utf8").digest("hex")
}

// ── Load validation ──────────────────────────────────────────────────────────

function validateInstructionProfile(profile: InstructionProfile): void {
  if (typeof profile !== "object" || profile === null || Array.isArray(profile)) {
    throw new TypeError("instructions must be an object")
  }
  for (const slot of INSTRUCTION_SLOTS) {
    const text = profile[slot]
    if (text === undefined) continue
    if (typeof text !== "string") throw new TypeError(`instructions.${slot} must be a string`)
    if (text.length > MAX_INSTRUCTION_CHARS) {
      throw new RangeError(`instructions.${slot} exceeds ${MAX_INSTRUCTION_CHARS} chars`)
    }
  }
}

function validateRuntimePatch(runtime: HarnessRuntimePatch): void {
  if (typeof runtime !== "object" || runtime === null) {
    throw new TypeError("manifest.runtime must be an object")
  }
  for (const [key, value] of Object.entries(runtime)) {
    if (!RUNTIME_PATCH_KEYS.includes(key)) {
      throw new RangeError(`runtime patch key not in the editable whitelist: ${key}`)
    }
    if (value !== undefined) validateRuntimeValue(key, value)
  }
}

/** Per-key value typing for runtime patches. An LLM proposer WILL eventually put instruction prose
 *  where a boolean belongs; rejecting it here turns a mid-run kernel `InvalidConfig` crash into a
 *  discardable candidate. */
function validateRuntimeValue(key: string, value: unknown): void {
  const positiveInt = (v: unknown) => typeof v === "number" && Number.isInteger(v) && v > 0
  switch (key) {
    case "maxTurns":
    case "maxTotalTokens":
    case "skillLeaseTurns":
    case "retrievalTopK":
    case "promotionRecallThreshold":
      if (!positiveInt(value)) throw new TypeError(`runtime.${key} must be a positive integer`)
      return
    case "criteriaGate":
      if (typeof value !== "boolean") throw new TypeError("runtime.criteriaGate must be a boolean")
      return
    case "knowledgeBudgetRatio":
      if (typeof value !== "number" || !(value > 0 && value <= 1)) {
        throw new TypeError("runtime.knowledgeBudgetRatio must be a number in (0, 1]")
      }
      return
    case "repeatFuse": {
      if (value === false) return
      if (typeof value !== "object" || value === null) {
        throw new TypeError("runtime.repeatFuse must be false or { denyAfter?, terminateAfter? }")
      }
      const fuse = value as Record<string, unknown>
      for (const k of Object.keys(fuse)) {
        if (k !== "denyAfter" && k !== "terminateAfter") {
          throw new RangeError(`runtime.repeatFuse has unknown key: ${k}`)
        }
        if (fuse[k] !== undefined && !positiveInt(fuse[k])) {
          throw new TypeError(`runtime.repeatFuse.${k} must be a positive integer`)
        }
      }
      return
    }
    case "entropyWatch": {
      if (typeof value !== "object" || value === null) {
        throw new TypeError("runtime.entropyWatch must be an object")
      }
      const watch = value as Record<string, unknown>
      for (const k of Object.keys(watch)) {
        const v = watch[k]
        if (v === undefined) continue
        if (k === "enabled" || k === "notifyModel") {
          if (typeof v !== "boolean") throw new TypeError(`runtime.entropyWatch.${k} must be a boolean`)
        } else if (k === "threshold" || k === "hysteresis") {
          if (typeof v !== "number" || !(v >= 0 && v <= 1)) {
            throw new TypeError(`runtime.entropyWatch.${k} must be a number in [0, 1]`)
          }
        } else if (k === "cooldownTurns") {
          if (!positiveInt(v)) throw new TypeError("runtime.entropyWatch.cooldownTurns must be a positive integer")
        } else {
          throw new RangeError(`runtime.entropyWatch has unknown key: ${k}`)
        }
      }
      return
    }
    default:
      throw new RangeError(`runtime patch key not in the editable whitelist: ${key}`)
  }
}

/** Structural load check — throws on anything a manifest is forbidden to carry. */
export function validateManifest(manifest: HarnessManifest): void {
  if (typeof manifest !== "object" || manifest === null) throw new TypeError("manifest must be an object")
  if (manifest.manifestVersion !== 1) throw new TypeError("manifest.manifestVersion must be 1")
  if (!(manifest.parent === null || typeof manifest.parent === "string")) {
    throw new TypeError("manifest.parent must be a digest string or null")
  }
  if (!Array.isArray(manifest.editableSurfaces) || manifest.editableSurfaces.some(s => typeof s !== "string")) {
    throw new TypeError("manifest.editableSurfaces must be a string[]")
  }
  if (manifest.scope !== undefined) {
    if (typeof manifest.scope !== "string" || !SCOPE_PATTERN.test(manifest.scope)) {
      throw new TypeError("manifest.scope must be a non-empty path-safe token matching /^[A-Za-z0-9._-]{1,64}$/")
    }
  }
  if (manifest.instructions !== undefined) validateInstructionProfile(manifest.instructions)
  if (manifest.nudges !== undefined) validateNudgeRules(manifest.nudges)
  if (manifest.runtime !== undefined) validateRuntimePatch(manifest.runtime)
}

// ── Apply ────────────────────────────────────────────────────────────────────

/**
 * Fold a validated manifest onto `base` runtime options. Instructions ride through as DATA — the
 * runner composes the system prompt once at option normalization so `run_started` and the kernel's
 * AddSystemMessage stay byte-identical. Runtime keys outside the whitelist throw.
 */
export function applyManifest(manifest: HarnessManifest, base: RuntimeOptions): RuntimeOptions {
  validateManifest(manifest)
  const out: RuntimeOptions = { ...base }
  if (manifest.instructions !== undefined) out.instructions = manifest.instructions
  if (manifest.nudges !== undefined) out.nudges = manifest.nudges
  if (manifest.runtime !== undefined) {
    for (const [key, value] of Object.entries(manifest.runtime)) {
      if (value === undefined) continue
      if (MEMORY_POLICY_PATCH_KEYS.includes(key as MemoryPolicyPatchKey)) {
        out.memoryPolicy = { ...out.memoryPolicy, [key]: value }
      } else {
        (out as unknown as Record<string, unknown>)[key] = value
      }
    }
  }
  return out
}

function validatePatchShape(patch: HarnessPatch): void {
  if (typeof patch !== "object" || patch === null) throw new TypeError("patch must be an object")
  if (typeof patch.targetSurface !== "string" || patch.targetSurface.length === 0) {
    throw new TypeError("patch.targetSurface must be a non-empty string")
  }
  if (patch.op !== "set" && patch.op !== "append" && patch.op !== "remove") {
    throw new TypeError(`patch.op must be set|append|remove, got ${String(patch.op)}`)
  }
  for (const field of ["rationale", "targetCluster", "expectedEffect"] as const) {
    if (typeof patch[field] !== "string" || patch[field].length === 0) {
      throw new TypeError(`patch.${field} must be a non-empty string`)
    }
  }
}

function editInstructionSlot(manifest: HarnessManifest, slot: string | undefined, patch: HarnessPatch): void {
  if (slot === undefined || !INSTRUCTION_SLOTS.includes(slot as InstructionSlot)) {
    throw new RangeError(`unknown instruction slot: ${patch.targetSurface}`)
  }
  if (patch.op === "append") throw new RangeError("append applies only to nudges")
  const key = slot as InstructionSlot
  if (patch.op === "remove") {
    if (manifest.instructions) delete manifest.instructions[key]
    return
  }
  if (typeof patch.value !== "string") throw new TypeError(`instructions.${key} set requires a string value`)
  if (patch.value.length > MAX_INSTRUCTION_CHARS) {
    throw new RangeError(`instructions.${key} exceeds ${MAX_INSTRUCTION_CHARS} chars`)
  }
  manifest.instructions = { ...(manifest.instructions ?? {}), [key]: patch.value }
}

function editNudges(manifest: HarnessManifest, patch: HarnessPatch): void {
  const current = manifest.nudges ?? []
  if (patch.op === "set") {
    const rules = patch.value as NudgeRule[]
    validateNudgeRules(rules)
    manifest.nudges = rules
    return
  }
  if (patch.op === "append") {
    const additions = Array.isArray(patch.value) ? (patch.value as NudgeRule[]) : [patch.value as NudgeRule]
    const merged = [...current, ...additions]
    validateNudgeRules(merged)
    manifest.nudges = merged
    return
  }
  // remove — by id
  if (typeof patch.value !== "string") throw new TypeError("nudges remove requires a rule id string")
  manifest.nudges = current.filter(rule => rule.id !== patch.value)
}

function editRuntime(manifest: HarnessManifest, key: string | undefined, patch: HarnessPatch): void {
  if (key === undefined || !RUNTIME_PATCH_KEYS.includes(key)) {
    throw new RangeError(`runtime patch key not in the editable whitelist: ${patch.targetSurface}`)
  }
  if (patch.op === "append") throw new RangeError("append applies only to nudges")
  const runtime: Record<string, unknown> = { ...(manifest.runtime ?? {}) }
  if (patch.op === "remove") delete runtime[key]
  else {
    validateRuntimeValue(key, patch.value)
    runtime[key] = patch.value
  }
  manifest.runtime = runtime as HarnessRuntimePatch
}

function applySurfaceEdit(manifest: HarnessManifest, patch: HarnessPatch): void {
  const [head, sub] = patch.targetSurface.split(".")
  if (head === "instructions") return editInstructionSlot(manifest, sub, patch)
  if (head === "nudges") {
    if (sub !== undefined) throw new RangeError(`nudges surface takes no sub-path: ${patch.targetSurface}`)
    return editNudges(manifest, patch)
  }
  if (head === "runtime") return editRuntime(manifest, sub, patch)
  throw new RangeError(`unknown surface path: ${patch.targetSurface}`)
}

/**
 * Apply one structural edit, returning a NEW manifest whose `parent` is the source's digest. Throws
 * when the surface is off-whitelist, the patch is malformed, or the result violates a bound
 * (instruction ≤4000 chars, nudge load rules). The source manifest is never mutated.
 */
export function applyPatch(manifest: HarnessManifest, patch: HarnessPatch): HarnessManifest {
  validatePatchShape(patch)
  if (!manifest.editableSurfaces.includes(patch.targetSurface)) {
    throw new RangeError(`surface not in the editable whitelist: ${patch.targetSurface}`)
  }
  const next = structuredClone(manifest) as HarnessManifest
  applySurfaceEdit(next, patch)
  next.parent = manifestDigest(manifest)
  validateManifest(next)
  return next
}
