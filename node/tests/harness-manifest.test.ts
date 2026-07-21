/**
 * Self-Harness editable surfaces — HarnessManifest as data.
 *  - manifestDigest: deterministic, key-order invariant, undefined-skipping.
 *  - applyPatch: whitelist + bound rejection, parent-chain linkage, no source mutation.
 *  - applyManifest: whitelisted fold onto RuntimeOptions, unknown-runtime-key rejection.
 *  - composeSystemPrompt: fixed order, byte stability, empty-slot skipping.
 */
import {
  composeSystemPrompt,
  manifestDigest,
  applyManifest,
  applyPatch,
  validateManifest,
  surfaceTier,
  type HarnessManifest,
  type HarnessPatch,
} from "../src/harness/manifest.js"
import type { RuntimeOptions } from "../src/runtime/runner.js"

function seed(): HarnessManifest {
  return {
    manifestVersion: 1,
    parent: null,
    instructions: { bootstrap: "boot", verification: "verify" },
    nudges: [],
    runtime: { maxTurns: 10 },
    editableSurfaces: [
      "instructions.bootstrap",
      "instructions.execution",
      "instructions.verification",
      "instructions.failureRecovery",
      "nudges",
      "runtime.maxTurns",
      "runtime.criteriaGate",
    ],
  }
}

function patch(over: Partial<HarnessPatch>): HarnessPatch {
  return { targetSurface: "instructions.bootstrap", op: "set", value: "x", rationale: "r", targetCluster: "c", expectedEffect: "e", ...over }
}

describe("manifestDigest", () => {
  it("is invariant to object key insertion order", () => {
    const a = seed()
    const b: HarnessManifest = {
      editableSurfaces: [...a.editableSurfaces], // same array order (arrays are ordered)
      runtime: { maxTurns: 10 },
      nudges: [],
      instructions: { verification: "verify", bootstrap: "boot" }, // slot keys reordered
      parent: null,
      manifestVersion: 1,
    }
    expect(manifestDigest(a)).toBe(manifestDigest(b))
  })

  it("ignores undefined-valued keys (no empty slots)", () => {
    const a = seed()
    const withUndef = { ...a, modelProfile: undefined } as HarnessManifest
    expect(manifestDigest(withUndef)).toBe(manifestDigest(a))
  })

  it("changes when any surface value changes", () => {
    const a = seed()
    const b: HarnessManifest = { ...a, instructions: { bootstrap: "boot2", verification: "verify" } }
    expect(manifestDigest(b)).not.toBe(manifestDigest(a))
  })

  it("is sensitive to editableSurfaces order (arrays are ordered)", () => {
    const a = seed()
    const b: HarnessManifest = { ...a, editableSurfaces: [...a.editableSurfaces].reverse() }
    expect(manifestDigest(b)).not.toBe(manifestDigest(a))
  })
})

describe("manifest scope", () => {
  // Golden digest of the pre-scope seed() with NO scope field. The scope type addition must not shift
  // it (canonicalJson skips undefined), or every existing lineage digest on disk would break.
  const ABSENT_SCOPE_DIGEST = "07fe15dd850dca52a9a8b82a68f11cc2350b8988d92fc6bc10e94e9f59a651ab"

  it("domain-separates the digest: scope a / b / absent are three distinct digests", () => {
    const absent = manifestDigest(seed())
    const a = manifestDigest({ ...seed(), scope: "a" })
    const b = manifestDigest({ ...seed(), scope: "b" })
    expect(new Set([absent, a, b]).size).toBe(3)
  })

  it("leaves a pre-scope (absent-scope) manifest digest byte-identical to the pre-change golden", () => {
    expect(manifestDigest(seed())).toBe(ABSENT_SCOPE_DIGEST)
  })

  it("rejects empty / path-hostile / non-string scope at load", () => {
    expect(() => validateManifest({ ...seed(), scope: "" })).toThrow(/scope/)
    expect(() => validateManifest({ ...seed(), scope: "../x" })).toThrow(/scope/)
    expect(() => validateManifest({ ...seed(), scope: "a/b" })).toThrow(/scope/)
    expect(() => validateManifest({ ...seed(), scope: 5 as unknown as string })).toThrow(/scope/)
    expect(() => validateManifest({ ...seed(), scope: "x".repeat(65) })).toThrow(/scope/)
  })

  it("accepts a well-formed scope and lets applyPatch's child inherit it", () => {
    const m: HarnessManifest = { ...seed(), scope: "tenant-42" }
    expect(() => validateManifest(m)).not.toThrow()
    const child = applyPatch(m, patch({ targetSurface: "instructions.bootstrap", value: "new boot" }))
    expect(child.scope).toBe("tenant-42")
  })

  it("refuses a patch targeting scope even when it is listed in editableSurfaces", () => {
    const m: HarnessManifest = { ...seed(), scope: "s", editableSurfaces: [...seed().editableSurfaces, "scope"] }
    expect(() => applyPatch(m, patch({ targetSurface: "scope", value: "other" }))).toThrow(/unknown surface path/)
    // scope untouched on the source.
    expect(m.scope).toBe("s")
  })
})

describe("composeSystemPrompt", () => {
  it("joins base then slots in fixed order, skipping empty slots", () => {
    const out = composeSystemPrompt("BASE", { failureRecovery: "FR", bootstrap: "BOOT", verification: "VER" })
    expect(out).toBe("BASE\n\nBOOT\n\nVER\n\nFR") // execution absent → skipped; order fixed
  })

  it("returns base unchanged when all slots are empty", () => {
    expect(composeSystemPrompt("BASE", {})).toBe("BASE")
    expect(composeSystemPrompt("BASE", undefined)).toBe("BASE")
    expect(composeSystemPrompt(undefined, undefined)).toBeUndefined()
  })

  it("drops an absent base", () => {
    expect(composeSystemPrompt(undefined, { bootstrap: "BOOT", execution: "EXE" })).toBe("BOOT\n\nEXE")
  })

  it("is byte-stable across slot key insertion order", () => {
    expect(composeSystemPrompt("B", { bootstrap: "x", execution: "y" }))
      .toBe(composeSystemPrompt("B", { execution: "y", bootstrap: "x" }))
  })
})

describe("applyPatch", () => {
  it("rejects a surface outside editableSurfaces", () => {
    expect(() => applyPatch(seed(), patch({ targetSurface: "runtime.entropyWatch", value: {} }))).toThrow(/whitelist/)
  })

  it("rejects an instruction over 4000 chars", () => {
    expect(() => applyPatch(seed(), patch({ targetSurface: "instructions.execution", value: "x".repeat(4001) }))).toThrow(/4000/)
  })

  it("accepts an instruction at exactly 4000 chars", () => {
    const next = applyPatch(seed(), patch({ targetSurface: "instructions.execution", value: "x".repeat(4000) }))
    expect(next.instructions?.execution).toHaveLength(4000)
  })

  it("rejects append on a non-nudge surface", () => {
    expect(() => applyPatch(seed(), patch({ targetSurface: "instructions.bootstrap", op: "append", value: "x" }))).toThrow(/append/)
  })

  it("rejects a malformed patch (missing rationale)", () => {
    expect(() => applyPatch(seed(), { targetSurface: "instructions.bootstrap", op: "set", value: "x", targetCluster: "c", expectedEffect: "e" } as unknown as HarnessPatch)).toThrow(/rationale/)
  })

  it("links parent to the source digest without mutating the source", () => {
    const m = seed()
    const before = manifestDigest(m)
    const child = applyPatch(m, patch({ targetSurface: "instructions.bootstrap", value: "new boot" }))
    expect(child.parent).toBe(before)
    expect(child.instructions?.bootstrap).toBe("new boot")
    // source untouched
    expect(m.instructions?.bootstrap).toBe("boot")
    expect(manifestDigest(m)).toBe(before)
    // grandchild chains onto the child
    const grand = applyPatch(child, patch({
      targetSurface: "nudges", op: "set",
      value: [{ id: "n1", on: { kind: "turns_at_least", count: 2 }, note: "keep going" }],
    }))
    expect(grand.parent).toBe(manifestDigest(child))
    expect(grand.nudges).toHaveLength(1)
  })

  it("appends and removes nudges by id", () => {
    const withRule = applyPatch(seed(), patch({
      targetSurface: "nudges", op: "append",
      value: { id: "n1", on: { kind: "tool_error" }, note: "oops" },
    }))
    expect(withRule.nudges).toHaveLength(1)
    const removed = applyPatch(withRule, patch({ targetSurface: "nudges", op: "remove", value: "n1" }))
    expect(removed.nudges).toHaveLength(0)
  })

  it("clears an instruction slot on remove", () => {
    const cleared = applyPatch(seed(), patch({ targetSurface: "instructions.verification", op: "remove" }))
    expect(cleared.instructions?.verification).toBeUndefined()
    expect(cleared.instructions?.bootstrap).toBe("boot")
  })
})

describe("applyManifest", () => {
  const base = {
    provider: { tag: "P" },
    maxTokens: 42,
    maxTurns: 3,
    memoryPolicy: { memoryPath: ".memory", validationEnabled: false },
  } as unknown as RuntimeOptions

  it("folds instructions, nudges, and whitelisted runtime keys onto base", () => {
    const m: HarnessManifest = {
      ...seed(),
      runtime: { maxTurns: 10, retrievalTopK: 5, promotionRecallThreshold: 2 },
    }
    const out = applyManifest(m, base)
    expect(out.maxTurns).toBe(10) // manifest runtime wins over base
    expect(out.memoryPolicy?.retrievalTopK).toBe(5)
    expect(out.memoryPolicy?.promotionRecallThreshold).toBe(2)
    expect(out.memoryPolicy?.memoryPath).toBe(".memory")
    expect(out.memoryPolicy?.validationEnabled).toBe(false)
    expect(out.instructions).toEqual(m.instructions)
    expect(out.nudges).toEqual(m.nudges)
    expect(out.maxTokens).toBe(42) // untouched base field survives
    expect((out.provider as unknown as { tag: string }).tag).toBe("P")
  })

  it("degrades to the whitelist: a non-whitelisted runtime key throws", () => {
    const m = { ...seed(), runtime: { governancePolicy: {} } as unknown as HarnessManifest["runtime"] }
    expect(() => applyManifest(m, base)).toThrow(/whitelist/)
  })

  it("does not mutate the base options", () => {
    const b = { ...base }
    applyManifest(seed(), b)
    expect(b.maxTurns).toBe(3)
    expect((b as RuntimeOptions).instructions).toBeUndefined()
  })
})

describe("validateManifest", () => {
  it("rejects a wrong version and a bad parent", () => {
    expect(() => validateManifest({ ...seed(), manifestVersion: 2 as unknown as 1 })).toThrow(/manifestVersion/)
    expect(() => validateManifest({ ...seed(), parent: 5 as unknown as string })).toThrow(/parent/)
  })

  it("rejects an unknown runtime key at load", () => {
    expect(() => validateManifest({ ...seed(), runtime: { skillLeaseTurns: 4, badKey: 1 } as unknown as HarnessManifest["runtime"] })).toThrow(/whitelist/)
  })
})

describe("runtime value typing", () => {
  // Live-run regression: a DeepSeek proposer set runtime.criteriaGate to instruction PROSE; the key
  // whitelist let it through and the kernel refused the run mid-flight. Values must type-check at
  // the patch/load boundary so the candidate dies as `apply_failed`, not as a loop crash.
  it("rejects a string where runtime.criteriaGate expects a boolean (patch path)", () => {
    const p = patch({ targetSurface: "runtime.criteriaGate", value: "Check the output for truncation." })
    expect(() => applyPatch(seed(), p)).toThrow(/boolean/)
    expect(applyPatch(seed(), patch({ targetSurface: "runtime.criteriaGate", value: false })).runtime?.criteriaGate).toBe(false)
  })

  it("rejects bad runtime values at load", () => {
    const bad = (runtime: unknown) =>
      validateManifest({ ...seed(), runtime: runtime as HarnessManifest["runtime"] })
    expect(() => bad({ criteriaGate: "yes" })).toThrow(/boolean/)
    expect(() => bad({ maxTurns: 2.5 })).toThrow(/positive integer/)
    expect(() => bad({ knowledgeBudgetRatio: 1.5 })).toThrow(/\(0, 1\]/)
    expect(() => bad({ retrievalTopK: 0 })).toThrow(/positive integer/)
    expect(() => bad({ promotionRecallThreshold: 1.5 })).toThrow(/positive integer/)
    expect(() => bad({ repeatFuse: { bogus: 1 } })).toThrow(/unknown key/)
    expect(() => bad({ entropyWatch: { threshold: "high" } })).toThrow(/\[0, 1\]/)
    expect(() => bad({ entropyWatch: { surprise: true } })).toThrow(/unknown key/)
  })

  it("accepts well-typed runtime values", () => {
    expect(() => validateManifest({
      ...seed(),
      runtime: {
        maxTurns: 8,
        criteriaGate: false,
        repeatFuse: { denyAfter: 3 },
        entropyWatch: { enabled: true, threshold: 0.7, cooldownTurns: 2 },
        knowledgeBudgetRatio: 0.25,
        retrievalTopK: 5,
        promotionRecallThreshold: 2,
      },
    })).not.toThrow()
  })

  it("accepts retrieval behavior patches on editable runtime surfaces", () => {
    const m: HarnessManifest = {
      ...seed(),
      editableSurfaces: ["runtime.retrievalTopK", "runtime.promotionRecallThreshold"],
    }
    const withTopK = applyPatch(m, patch({ targetSurface: "runtime.retrievalTopK", value: 7 }))
    const withRecall = applyPatch(withTopK, patch({ targetSurface: "runtime.promotionRecallThreshold", value: 3 }))
    expect(withRecall.runtime?.retrievalTopK).toBe(7)
    expect(withRecall.runtime?.promotionRecallThreshold).toBe(3)
  })
})

// ── tool/skill editable surfaces ──────────────────────────────────────
describe("tool/skill surface typing + bounds", () => {
  const bad = (runtime: unknown) => validateManifest({ ...seed(), runtime: runtime as HarnessManifest["runtime"] })

  it("accepts well-typed tool/skill surfaces", () => {
    expect(() => validateManifest({
      ...seed(),
      runtime: {
        allowedToolIds: ["read", "search"],
        stableCoreToolIds: ["read"],
        enablePlanTool: true,
        skillFilter: ["debug"],
      },
    })).not.toThrow()
  })

  it("rejects an EMPTY allowedToolIds / stableCoreToolIds (empty ⇒ runner reads as no-gating = WIDEN)", () => {
    expect(() => bad({ allowedToolIds: [] })).toThrow(/non-empty/)
    expect(() => bad({ stableCoreToolIds: [] })).toThrow(/non-empty/)
  })

  it("ACCEPTS an empty skillFilter (empty ⇒ no skills, a legitimate narrowing)", () => {
    expect(() => bad({ skillFilter: [] })).not.toThrow()
  })

  it("rejects non-string, duplicate, over-long, or oversized id lists", () => {
    expect(() => bad({ allowedToolIds: ["ok", 5] })).toThrow(/non-empty strings/)
    expect(() => bad({ allowedToolIds: ["dup", "dup"] })).toThrow(/unique/)
    expect(() => bad({ skillFilter: ["dup", "dup"] })).toThrow(/unique/)
    expect(() => bad({ allowedToolIds: [""] })).toThrow(/non-empty strings/)
    expect(() => bad({ allowedToolIds: ["x".repeat(129)] })).toThrow(/128 chars/)
    expect(() => bad({ allowedToolIds: Array.from({ length: 129 }, (_, i) => `t${i}`) })).toThrow(/128 entries/)
  })

  it("rejects a non-boolean enablePlanTool", () => {
    expect(() => bad({ enablePlanTool: "yes" })).toThrow(/boolean/)
  })

  it("enforces stableCoreToolIds ⊆ allowedToolIds within one manifest", () => {
    expect(() => bad({ allowedToolIds: ["a", "b"], stableCoreToolIds: ["a", "c"] })).toThrow(/subset|ceiling/)
    // subset ok; either absent ⇒ no cross-check
    expect(() => bad({ allowedToolIds: ["a", "b"], stableCoreToolIds: ["a"] })).not.toThrow()
    expect(() => bad({ stableCoreToolIds: ["a"] })).not.toThrow()
  })

  it("applyPatch rejects an empty allowedToolIds set at the structural gate (→ apply_failed)", () => {
    const m: HarnessManifest = { ...seed(), editableSurfaces: [...seed().editableSurfaces, "runtime.allowedToolIds"] }
    expect(() => applyPatch(m, patch({ targetSurface: "runtime.allowedToolIds", value: [] }))).toThrow(/non-empty/)
    expect(applyPatch(m, patch({ targetSurface: "runtime.allowedToolIds", value: ["read"] })).runtime?.allowedToolIds).toEqual(["read"])
  })
})

describe("applyManifest intersection fold (capability ceiling)", () => {
  const baseWith = (over: Record<string, unknown>) => ({ maxTokens: 1, ...over } as unknown as RuntimeOptions)

  it("intersects with a NON-EMPTY host baseline (effective = manifest ∩ base, manifest order)", () => {
    const m: HarnessManifest = { ...seed(), runtime: { allowedToolIds: ["c", "a", "z"] } }
    const out = applyManifest(m, baseWith({ allowedToolIds: ["a", "b", "c"] }))
    expect(out.allowedToolIds).toEqual(["c", "a"]) // z dropped (not in base); order follows manifest
  })

  it("returns the manifest list verbatim when the host baseline is unset OR empty (both mean 'all tools')", () => {
    const m: HarnessManifest = { ...seed(), runtime: { allowedToolIds: ["a", "b"] } }
    expect(applyManifest(m, baseWith({})).allowedToolIds).toEqual(["a", "b"]) // unset host ⇒ universe
    expect(applyManifest(m, baseWith({ allowedToolIds: [] })).allowedToolIds).toEqual(["a", "b"]) // empty host ⇒ universe
  })

  it("THROWS on an empty allowedToolIds / stableCoreToolIds intersection, naming both operands", () => {
    const m: HarnessManifest = { ...seed(), runtime: { allowedToolIds: ["x", "y"] } }
    expect(() => applyManifest(m, baseWith({ allowedToolIds: ["a", "b"] }))).toThrow(/allowedToolIds intersection is empty[\s\S]*x, y[\s\S]*a, b/)
    const s: HarnessManifest = { ...seed(), runtime: { stableCoreToolIds: ["x"] } }
    expect(() => applyManifest(s, baseWith({ stableCoreToolIds: ["a"] }))).toThrow(/stableCoreToolIds intersection is empty/)
  })

  it("skillFilter: an EMPTY intersection is fine (= no skills), and a present [] baseline intersects", () => {
    const m: HarnessManifest = { ...seed(), runtime: { skillFilter: ["x", "y"] } }
    expect(applyManifest(m, baseWith({ skillFilter: ["a", "b"] })).skillFilter).toEqual([]) // disjoint ⇒ empty, no throw
    expect(applyManifest(m, baseWith({ skillFilter: [] })).skillFilter).toEqual([]) // [] host is a real ceiling
    expect(applyManifest(m, baseWith({})).skillFilter).toEqual(["x", "y"]) // absent host ⇒ all skills
  })

  it("enablePlanTool folds by plain assignment (both directions)", () => {
    expect(applyManifest({ ...seed(), runtime: { enablePlanTool: true } }, baseWith({ enablePlanTool: false })).enablePlanTool).toBe(true)
    expect(applyManifest({ ...seed(), runtime: { enablePlanTool: false } }, baseWith({ enablePlanTool: true })).enablePlanTool).toBe(false)
  })

  it("NEVER-WIDEN property: whenever the host baseline was set, the effective set ⊆ it", () => {
    const base = ["a", "b", "c", "d"]
    for (const manifestList of [["a"], ["b", "d"], ["a", "b", "c"], ["a", "x"], ["c", "b", "a"]]) {
      for (const key of ["allowedToolIds", "stableCoreToolIds", "skillFilter"] as const) {
        const out = applyManifest({ ...seed(), runtime: { [key]: manifestList } }, baseWith({ [key]: base }))
        const eff = (out as unknown as Record<string, string[]>)[key] ?? []
        for (const id of eff) expect(base).toContain(id) // effective ⊆ host base — never a new capability
      }
    }
  })
})

// ── promotion tiers ────────────────────────────────────────────────────
describe("surfaceTier (promotion tiers)", () => {
  // Every whitelisted runtime.* surface is Tier A (auto): typed validation + the ceiling invariant guard it.
  const RUNTIME_SURFACES = [
    "maxTurns", "maxTotalTokens", "criteriaGate", "repeatFuse", "entropyWatch",
    "knowledgeBudgetRatio", "skillLeaseTurns", "allowedToolIds", "stableCoreToolIds",
    "enablePlanTool", "skillFilter", "retrievalTopK", "promotionRecallThreshold",
  ]

  it("maps every runtime.* whitelist surface to auto", () => {
    for (const key of RUNTIME_SURFACES) expect(surfaceTier(`runtime.${key}`)).toBe("auto")
  })

  it("maps instructions.* and nudges to screened (free text ⇒ injection screen)", () => {
    for (const slot of ["bootstrap", "execution", "verification", "failureRecovery"]) {
      expect(surfaceTier(`instructions.${slot}`)).toBe("screened")
    }
    expect(surfaceTier("nudges")).toBe("screened")
  })

  it("throws on an unknown surface, an unknown slot / runtime key, and a mis-shaped path", () => {
    expect(() => surfaceTier("governance.limits")).toThrow(/unknown surface path/)
    expect(() => surfaceTier("scope")).toThrow(/unknown surface path/)
    expect(() => surfaceTier("instructions.bogus")).toThrow(/instruction slot/)
    expect(() => surfaceTier("runtime.governancePolicy")).toThrow(/whitelist/)
    expect(() => surfaceTier("nudges.extra")).toThrow(/no sub-path/)
  })

  it("never returns human today — no capability-widening surface is expressible", () => {
    const all = [
      "instructions.bootstrap", "instructions.execution", "instructions.verification",
      "instructions.failureRecovery", "nudges",
      ...RUNTIME_SURFACES.map(k => `runtime.${k}`),
    ]
    for (const s of all) expect(surfaceTier(s)).not.toBe("human")
  })
})
