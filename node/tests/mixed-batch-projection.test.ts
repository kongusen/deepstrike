/**
 * S5 · D-A mixed-batch fixtures — node projection-layer consumer (0.2.64).
 *
 * The same multi-step sequence fixtures the rust driver replays
 * (tests/fixtures/mixed-batch/, runner: tests/rust/src/t17_mixed_batch_sequences.rs)
 * are fed through the native kernel binding here to pin the projection layer's
 * first-effect semantics: a step's published manifest is the kernel's publication
 * order, and the current host action is always its FIRST effect — the one-effect-
 * per-step consumption contract that M3's withholding exists to protect.
 */
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"

import { getKernel } from "../src/kernel.js"

const FIXTURE_DIR = join(process.cwd(), "../tests/fixtures/mixed-batch")

interface SequenceStep {
  input: Record<string, unknown>
  expect: {
    published_kinds: string[]
    withheld_kinds?: string[]
    rederived?: string[]
  }
}

interface SequenceFixture {
  id: string
  kind: string
  steps: SequenceStep[]
}

function loadFixtures(): SequenceFixture[] {
  return readdirSync(FIXTURE_DIR)
    .filter(name => name.endsWith(".json") && name !== "schema.json")
    .sort()
    .map(name => JSON.parse(readFileSync(join(FIXTURE_DIR, name), "utf8")) as SequenceFixture)
}

describe("mixed-batch sequence fixtures — projection layer", () => {
  for (const fixture of loadFixtures()) {
    test(`${fixture.id}: every step projects its first published effect`, () => {
      expect(fixture.kind).toBe("mixed_batch_sequence")
      const native = getKernel()
      const kernel = new native.CanonicalKernel()
      const withheldSoFar = new Set<string>()

      fixture.steps.forEach((step, index) => {
        const where = `${fixture.id} step ${index}`
        const preparation = kernel.prepare(JSON.stringify(step.input))
        if (preparation.status !== "prepared") {
          throw new Error(
            `${where}: expected a prepared step, got ${preparation.status}: ${
              preparation.status === "rejected" ? preparation.faultJson : ""
            }`,
          )
        }
        const commit = kernel.commit(preparation.prepareToken, preparation.recordDigest)

        const manifest = JSON.parse(kernel.publishedEffectsManifestJson(commit.plannedStepJson)) as Array<{
          effect_id: string
          kind: string
        }>
        const published = manifest.map(entry => entry.kind)
        expect({ where, published }).toEqual({ where, published: step.expect.published_kinds })

        for (const kind of step.expect.withheld_kinds ?? []) {
          expect({ where, kind, coPublished: published.includes(kind) }).toEqual({ where, kind, coPublished: false })
          withheldSoFar.add(kind)
        }
        for (const kind of step.expect.rederived ?? []) {
          expect(published).toContain(kind)
          expect({ where, kind, previouslyWithheld: withheldSoFar.has(kind) }).toEqual({
            where,
            kind,
            previouslyWithheld: true,
          })
        }

        // 取首 effect: the current action is the manifest's head, never a later sibling.
        const projection = JSON.parse(kernel.projectPlannedStepJson(commit.plannedStepJson)) as
          | { state: "idle" }
          | { state: "action"; action: { kind: string; effect_id: string } }
          | { state: "terminal" }
        if (manifest.length === 0) {
          expect({ where, state: projection.state }).toEqual({ where, state: "idle" })
        } else {
          if (projection.state !== "action") {
            throw new Error(`${where}: a step with published effects must project an action`)
          }
          expect({ where, projected: projection.action.effect_id }).toEqual({
            where,
            projected: manifest[0].effect_id,
          })
          expect(projection.action.kind).toBe(manifest[0].kind)
        }
      })
    })
  }
})
