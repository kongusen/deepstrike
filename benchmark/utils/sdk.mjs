/**
 * SDK loader + provider resolution.
 *
 * Loads the Node SDK from a compiled dist (matches tool-gating-dwell.mjs convention) so the
 * benchmark tree stays pure .mjs and doesn't need its own tsc step.
 *
 * `resolveProvider` reads env vars and returns a { provider, apiKey, model, baseURL, endpoint }
 * descriptor — the same shape `createProvider({ provider, apiKey, model, ... })` accepts. Centralised
 * so every scenario gets identical provider resolution.
 */

import { existsSync } from "node:fs"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const __dir = path.dirname(fileURLToPath(import.meta.url))
const benchRoot = path.resolve(__dir, "..")
export const repoRoot = path.resolve(benchRoot, "..")
export const nodeRoot = path.join(repoRoot, "node")

/** @returns {Promise<any>} */
export async function loadSdk() {
  const p = path.join(nodeRoot, "dist", "index.js")
  if (!existsSync(p)) {
    throw new Error(`Node SDK dist not found at ${p}. Run: npm run build --prefix node`)
  }
  const root = await import(pathToFileURL(p).href)
  // judge()/AttemptLoop live on the @deepstrike/sdk/harness subpath since the H2 harness
  // unification; merge them in so runner code can keep destructuring one namespace.
  const harnessPath = path.join(nodeRoot, "dist", "harness", "public.js")
  if (!existsSync(harnessPath)) return root
  const harness = await import(pathToFileURL(harnessPath).href)
  return { ...root, ...harness }
}

/**
 * @typedef {Object} ProviderDescriptor
 * @property {string} provider     "openai" / "deepseek" / "anthropic" / "minimax"
 * @property {string} apiKey
 * @property {string} model
 * @property {string} [baseURL]
 * @property {string} [endpoint]   Catalog endpoint id (e.g. "deepseek.openai")
 */

const PROVIDER_REGISTRY = {
  openai: () => ({
    provider: "openai",
    apiKey: process.env.OPENAI_API_KEY,
    model: process.env.OPENAI_MODEL || "gpt-4o-mini",
    baseURL: process.env.OPENAI_BASE_URL || undefined,
  }),
  deepseek: () => ({
    provider: "deepseek",
    apiKey: process.env.DEEPSEEK_API_KEY,
    model: process.env.DEEPSEEK_MODEL || "deepseek-chat",
    endpoint: "deepseek.openai",
  }),
  glm: () => ({
    provider: "glm",
    apiKey: process.env.GLM_API_KEY,
    model: process.env.GLM_MODEL || "glm-5.2",
    endpoint: "glm.openai",
  }),
  kimi: () => ({
    provider: "kimi",
    apiKey: process.env.KIMI_API_KEY,
    model: process.env.KIMI_MODEL || "kimi-k2.6",
    endpoint: "kimi.openai",
  }),
  anthropic: () => ({
    provider: "anthropic",
    apiKey: process.env.ANTHROPIC_API_KEY,
    model: process.env.ANTHROPIC_MODEL || "claude-sonnet-4-6",
  }),
  minimax: () => ({
    provider: "minimax",
    apiKey: process.env.MINIMAX_API_KEY,
    model: process.env.MINIMAX_MODEL || "MiniMax-Text-01",
  }),
}

/**
 * Resolve a provider descriptor by id, reading env vars.
 * @param {string} providerId
 * @returns {ProviderDescriptor}
 */
export function resolveProvider(providerId) {
  const id = String(providerId || process.env.LLM_PROVIDER || "openai").toLowerCase()
  const builder = PROVIDER_REGISTRY[id]
  if (!builder) {
    throw new Error(`Unknown provider: ${id}. Known: ${Object.keys(PROVIDER_REGISTRY).join(", ")}`)
  }
  const desc = builder()
  if (!desc.apiKey) {
    throw new Error(`Missing API key for ${id} — set ${id.toUpperCase()}_API_KEY in .env`)
  }
  return /** @type {ProviderDescriptor} */ (desc)
}

/** Available provider ids. */
export const PROVIDER_IDS = Object.keys(PROVIDER_REGISTRY)
