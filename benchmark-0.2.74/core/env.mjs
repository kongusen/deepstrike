import { readFile } from "node:fs/promises"
import { resolve } from "node:path"

/** Load project .env entries without logging values. Existing process variables win. */
export async function loadProjectEnv(repoRoot) {
  const path = resolve(repoRoot, ".env")
  let source
  try {
    source = await readFile(path, "utf8")
  } catch {
    return { path, loaded: false, keys: [] }
  }
  const keys = []
  for (const line of source.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (!trimmed || trimmed.startsWith("#")) continue
    const match = trimmed.match(/^(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/)
    if (!match) continue
    const [, key, raw] = match
    const value = raw.trim().replace(/^("|')(.*)\1$/, "$2")
    keys.push(key)
    if (!(key in process.env)) process.env[key] = value
  }
  return { path, loaded: true, keys: [...new Set(keys)].sort() }
}
