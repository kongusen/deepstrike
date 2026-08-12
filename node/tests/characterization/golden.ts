/**
 * spc_013-A-00: golden-file helper for the characterization baseline.
 *
 * Golden files live in `__golden__/<name>.json` next to this helper. Re-bless with
 * `BLESS_GOLDEN=1` — blessing is the ONLY way goldens change; a diff otherwise fails the test,
 * which is the whole point (any adapter/registry refactor card must stop and justify a golden
 * diff as an intended behavior change before re-blessing).
 *
 * `stableStringify` sorts object keys recursively so goldens don't churn on insertion-order
 * accidents; array order is preserved (it's semantically meaningful in request bodies/events).
 */
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const GOLDEN_DIR = join(dirname(fileURLToPath(import.meta.url)), "__golden__")

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys)
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
        .map(([k, v]) => [k, sortKeys(v)]),
    )
  }
  return value
}

export function stableStringify(value: unknown): string {
  return JSON.stringify(sortKeys(value), null, 2) + "\n"
}

export function expectGolden(name: string, actual: unknown): void {
  const text = stableStringify(actual)
  const path = join(GOLDEN_DIR, `${name}.json`)
  if (process.env.BLESS_GOLDEN) {
    mkdirSync(GOLDEN_DIR, { recursive: true })
    writeFileSync(path, text)
    return
  }
  let expected: string
  try {
    expected = readFileSync(path, "utf8")
  } catch {
    throw new Error(`characterization golden missing: ${path} — create it with BLESS_GOLDEN=1`)
  }
  expect(text).toBe(expected)
}
