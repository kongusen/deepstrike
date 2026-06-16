/**
 * Scenario registry.
 *
 * Add new scenarios here. The CLI looks them up by `id`. Keep the file lazy where possible — every
 * import that touches the SDK runs at module-load time and the dwell scenario already does this,
 * so the list is short-and-static for now.
 *
 * @typedef {import("../core/scenario.mjs").BenchScenario} BenchScenario
 */

import { gatingDwellScenario } from "./gating-dwell.mjs"
import { compressionStressScenario } from "./compression-stress.mjs"
import { governanceWriteDenyScenario } from "./governance-write-deny.mjs"

/** @type {BenchScenario[]} */
export const SCENARIOS = [
  gatingDwellScenario,
  compressionStressScenario,
  governanceWriteDenyScenario,
]

/** @param {string} id @returns {BenchScenario | undefined} */
export function findScenario(id) {
  return SCENARIOS.find(s => s.id === id)
}

/** @returns {Array<{ id: string, description: string, variants: string[] }>} */
export function listScenarios() {
  return SCENARIOS.map(s => ({
    id: s.id,
    description: s.description,
    variants: s.variantOrder ?? Object.keys(s.variants),
  }))
}
