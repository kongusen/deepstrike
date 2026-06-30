#!/usr/bin/env node
/**
 * Symbol manifest extractor — dumps the live public-symbol surface of each SDK
 * straight from source, so docs-drift checks compare against reality, not a
 * hand-maintained list. Regenerated each run; never goes stale.
 *
 * Standalone:  node scripts/extract-symbol-manifest.mjs [--json]
 * As a module: import { buildManifest } from "./extract-symbol-manifest.mjs"
 *
 * Sources of truth (see .local-docs/specs/docs-code-narrative-sync.md §4):
 *   node    node/src/index.ts                       export {…} / export class|function|const|type
 *   python  python/deepstrike/__init__.py           __all__
 *   rust    crates/deepstrike-core/src/lib.rs       pub use {…} / pub struct|enum|trait|fn|type
 *   fields  python/deepstrike/runtime/runner.py     RuntimeOptions dataclass field names
 */
import { readFileSync, existsSync } from "node:fs"
import { join } from "node:path"

export const ROOT = new URL("..", import.meta.url).pathname

function read(root, rel) {
  const file = join(root, rel)
  return existsSync(file) ? readFileSync(file, "utf8") : null
}

/** Names exported from a TS barrel: re-exports, declarations, and types. */
export function extractNode(root) {
  const text = read(root, "node/src/index.ts")
  if (text == null) return { source: "node/src/index.ts", present: false, symbols: [] }
  const names = new Set()
  // export { a, b as c } from "..."   and   export type { X } from "..."
  for (const m of text.matchAll(/export\s+(?:type\s+)?\{([^}]*)\}/g)) {
    for (let part of m[1].split(",")) {
      part = part.trim()
      if (!part) continue
      const asMatch = part.match(/\bas\s+([A-Za-z0-9_$]+)$/)
      names.add(asMatch ? asMatch[1] : part.replace(/^type\s+/, "").trim())
    }
  }
  // export (async) function|class|const|let|var X   /   export interface|type X
  for (const m of text.matchAll(
    /export\s+(?:async\s+)?(?:abstract\s+)?(?:function|class|const|let|var|interface|enum)\s+([A-Za-z0-9_$]+)/g
  )) names.add(m[1])
  for (const m of text.matchAll(/export\s+type\s+([A-Za-z0-9_$]+)\s*=/g)) names.add(m[1])
  return { source: "node/src/index.ts", present: true, symbols: [...names].sort() }
}

/** Names listed in python __all__. */
export function extractPython(root) {
  const text = read(root, "python/deepstrike/__init__.py")
  if (text == null) return { source: "python/deepstrike/__init__.py", present: false, symbols: [] }
  const start = text.indexOf("__all__")
  const names = new Set()
  if (start !== -1) {
    const open = text.indexOf("[", start)
    const close = text.indexOf("]", open)
    if (open !== -1 && close !== -1) {
      for (const m of text.slice(open, close).matchAll(/["']([A-Za-z0-9_]+)["']/g)) names.add(m[1])
    }
  }
  return { source: "python/deepstrike/__init__.py", present: true, symbols: [...names].sort() }
}

/** Public names re-exported or declared in the core crate root. */
export function extractRust(root) {
  const text = read(root, "crates/deepstrike-core/src/lib.rs")
  if (text == null) return { source: "crates/deepstrike-core/src/lib.rs", present: false, symbols: [] }
  const names = new Set()
  // pub use path::{A, B as C, D};
  for (const m of text.matchAll(/pub\s+use\s+[^;{]*\{([^}]*)\}/g)) {
    for (let part of m[1].split(",")) {
      part = part.trim().replace(/^self\b/, "")
      if (!part) continue
      const asMatch = part.match(/\bas\s+([A-Za-z0-9_]+)$/)
      names.add(asMatch ? asMatch[1] : part.split("::").pop().trim())
    }
  }
  // pub use a::b::C;   (single, no braces)
  for (const m of text.matchAll(/pub\s+use\s+([A-Za-z0-9_:]+)\s*;/g)) names.add(m[1].split("::").pop())
  // pub struct|enum|trait|fn|type|mod|const X
  for (const m of text.matchAll(
    /pub\s+(?:struct|enum|trait|fn|type|mod|const)\s+([A-Za-z0-9_]+)/g
  )) names.add(m[1])
  names.delete("")
  return { source: "crates/deepstrike-core/src/lib.rs", present: true, symbols: [...names].sort() }
}

/** Field names of a Python dataclass by name (e.g. RuntimeOptions). */
export function extractDataclassFields(root, rel, className) {
  const text = read(root, rel)
  if (text == null) return { source: rel, className, present: false, fields: [] }
  const classRe = new RegExp(`class\\s+${className}\\b[^:]*:`)
  const cm = text.match(classRe)
  if (!cm) return { source: rel, className, present: false, fields: [] }
  const bodyStart = text.indexOf(cm[0]) + cm[0].length
  // class body ends at the next top-level `class ` / `def ` at column 0
  const rest = text.slice(bodyStart)
  const endRel = rest.search(/\n(?:class |def |@)[A-Za-z_]/)
  const body = endRel === -1 ? rest : rest.slice(0, endRel)
  const fields = new Set()
  for (const line of body.split("\n")) {
    // match `  name: type ...` — a field decl, indented, not a method/comment
    const fm = line.match(/^\s{2,}([a-z_][A-Za-z0-9_]*)\s*:/)
    if (fm && !line.trim().startsWith("#")) fields.add(fm[1])
  }
  return { source: rel, className, present: true, fields: [...fields].sort() }
}

export function buildManifest(root = ROOT) {
  return {
    node: extractNode(root),
    python: extractPython(root),
    rust: extractRust(root),
    fields: {
      "python:RuntimeOptions": extractDataclassFields(
        root,
        "python/deepstrike/runtime/runner.py",
        "RuntimeOptions"
      ),
    },
  }
}

// CLI
if (import.meta.url === `file://${process.argv[1]}`) {
  const manifest = buildManifest()
  if (process.argv.includes("--json")) {
    console.log(JSON.stringify(manifest, null, 2))
  } else {
    for (const [sdk, m] of Object.entries(manifest)) {
      if (sdk === "fields") continue
      console.log(`${sdk.padEnd(7)} ${m.present ? m.symbols.length : "MISSING"} symbols  (${m.source})`)
    }
    for (const [k, f] of Object.entries(manifest.fields)) {
      console.log(`${k.padEnd(28)} ${f.present ? f.fields.length : "MISSING"} fields`)
    }
  }
}
