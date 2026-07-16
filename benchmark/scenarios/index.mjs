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
import { memoryRecallScenario } from "./memory-recall.mjs"
import { signalInjectionScenario } from "./signal-injection.mjs"
import { prefixCacheScenario } from "./prefix-cache.mjs"
import {
  orchestrationF1Scenario,
  orchestrationF2Scenario,
  orchestrationF3Scenario,
} from "./orchestration-scheduler.mjs"

/** @type {BenchScenario[]} */
export const SCENARIOS = [
  gatingDwellScenario,
  compressionStressScenario,
  governanceWriteDenyScenario,
  memoryRecallScenario,
  signalInjectionScenario,
  prefixCacheScenario,
  orchestrationF1Scenario,
  orchestrationF2Scenario,
  orchestrationF3Scenario,
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
