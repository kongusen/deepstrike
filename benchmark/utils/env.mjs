/**
 * Minimal .env loader — matches the shape used by tool-gating-dwell.mjs and run-e2e.mjs so the
 * benchmark CLI reads the same credentials the existing scripts do.
 */

import { existsSync, readFileSync } from "node:fs"

/** @param {string} fp */
export function loadEnvFile(fp) {
  if (!existsSync(fp)) return
  for (const raw of readFileSync(fp, "utf8").split(/\r?\n/)) {
    const line = raw.trim()
    if (!line || line.startsWith("#")) continue
    const norm = line.startsWith("export ") ? line.slice(7).trim() : line
    const eq = norm.indexOf("=")
    if (eq <= 0) continue
    const k = norm.slice(0, eq).trim()
    let v = norm.slice(eq + 1).trim()
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) {
      v = v.slice(1, -1)
    }
    if (!(k in process.env)) process.env[k] = v
  }
}

/** Redact provider keys from any string before logging. @param {string} s */
export function redact(s) {
  return String(s).replace(/sk-[a-zA-Z0-9_*.-]{6,}/g, "sk-[redacted]")
}
