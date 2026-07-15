/**
 * Capability suite registry.
 */

import { createBfclAdapter } from "../adapters/bfcl/load.mjs"
import { createGaiaAdapter } from "../adapters/gaia/load.mjs"
import { createWebArenaAdapter } from "../adapters/webarena/stub.mjs"

/** @type {Map<string, () => import("../core/types.mjs").CapAdapter>} */
const REGISTRY = new Map([
  ["bfcl", createBfclAdapter],
  ["gaia", createGaiaAdapter],
  ["webarena", createWebArenaAdapter],
])

/** @returns {import("../core/types.mjs").CapAdapter[]} */
export function listAdapters() {
  return [...REGISTRY.values()].map(fn => fn())
}

/** @param {string} id @returns {import("../core/types.mjs").CapAdapter | undefined} */
export function getAdapter(id) {
  const fn = REGISTRY.get(String(id || "").toLowerCase())
  return fn ? fn() : undefined
}

export function adapterIds() {
  return [...REGISTRY.keys()]
}
