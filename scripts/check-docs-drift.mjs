#!/usr/bin/env node
/**
 * Docs ↔ code drift checker — keeps the docs/ narrative grounded in real code.
 * Mirrors scripts/check-sdk-parity.mjs. Exit 0 when clean, 1 on any drift.
 *
 *   node scripts/check-docs-drift.mjs [--json] [--locale en|zh|both]
 *
 * Tracks (see .local-docs/specs/docs-code-narrative-sync.md):
 *   A · paths    every crates|python|node|wasm|rust …(.rs|.py|.ts) path in docs exists
 *   B · symbols  docs declaring `code_refs:` frontmatter resolve against the live
 *                symbol manifest (Phase 2 — no-op until docs add code_refs)
 *   C · parity   every zh doc has a docs/en/ counterpart (and no en orphans)
 */
import { readFileSync, existsSync, readdirSync, statSync } from "node:fs"
import { join, relative } from "node:path"
import { buildManifest } from "./extract-symbol-manifest.mjs"

const ROOT = new URL("..", import.meta.url).pathname
const DOCS = join(ROOT, "docs")
const args = process.argv.slice(2)
const JSON_OUT = args.includes("--json")

const SKIP_DIRS = new Set([".vitepress", "public", "wiki", "node_modules"])
const SKIP_FILES = new Set(["README.md"]) // sync-docs-to-wiki.py skips these too

/** All markdown docs, repo-relative, excluding build/skip dirs. */
function listDocs() {
  const out = []
  function walk(dir) {
    for (const name of readdirSync(dir)) {
      if (SKIP_DIRS.has(name)) continue
      const full = join(dir, name)
      if (statSync(full).isDirectory()) walk(full)
      else if (name.endsWith(".md")) out.push(relative(ROOT, full))
    }
  }
  walk(DOCS)
  return out.sort()
}

const PATH_RE = /(crates\/deepstrike-core\/src|python\/deepstrike|node\/src|wasm\/src|rust\/src)[A-Za-z0-9_/.\-]+\.(rs|py|ts)/g

/** Track A — referenced source paths must exist. */
function checkPaths(docs) {
  const fails = []
  let count = 0
  for (const doc of docs) {
    const lines = readFileSync(join(ROOT, doc), "utf8").split("\n")
    lines.forEach((line, i) => {
      for (const m of line.matchAll(PATH_RE)) {
        count++
        const p = m[0]
        if (!existsSync(join(ROOT, p))) fails.push({ doc, line: i + 1, ref: p })
      }
    })
  }
  return { total: count, fails }
}

/** Minimal frontmatter `code_refs` reader — returns null if absent. */
function readCodeRefs(text) {
  if (!text.startsWith("---")) return null
  const end = text.indexOf("\n---", 3)
  if (end === -1) return null
  const fm = text.slice(3, end)
  if (!/^\s*code_refs\s*:/m.test(fm)) return null
  // tolerant parse of the shape documented in the spec (§3 Track B)
  const refs = { node: [], python: [], rust: [], wasm: [], fields: {} }
  const lines = fm.split("\n")
  let mode = null
  for (const raw of lines) {
    const sdk = raw.match(/^\s{2,}(node|python|rust|wasm)\s*:\s*\[([^\]]*)\]/)
    if (sdk) {
      refs[sdk[1]] = sdk[2].split(",").map(s => s.trim().replace(/^["']|["']$/g, "")).filter(Boolean)
      mode = null
      continue
    }
    if (/^\s{2,}fields\s*:/.test(raw)) { mode = "fields"; continue }
    if (mode === "fields") {
      const fm2 = raw.match(/^\s{4,}["']?([A-Za-z0-9_:]+)["']?\s*:\s*\[([^\]]*)\]/)
      if (fm2) refs.fields[fm2[1]] = fm2[2].split(",").map(s => s.trim().replace(/^["']|["']$/g, "")).filter(Boolean)
      else if (/^\s{0,2}\S/.test(raw)) mode = null
    }
  }
  return refs
}

/** Track B — declared code_refs resolve against the live manifest. */
function checkSymbols(docs, manifest) {
  const fails = []
  let contracts = 0
  let count = 0
  for (const doc of docs) {
    const refs = readCodeRefs(readFileSync(join(ROOT, doc), "utf8"))
    if (!refs) continue
    contracts++
    for (const sdk of ["node", "python", "rust", "wasm"]) {
      const live = manifest[sdk]?.symbols ?? []
      for (const sym of refs[sdk]) {
        count++
        if (!live.includes(sym)) fails.push({ doc, sdk, ref: sym })
      }
    }
    for (const [key, wanted] of Object.entries(refs.fields)) {
      const live = manifest.fields[key]?.fields ?? []
      for (const f of wanted) {
        count++
        if (!live.includes(f)) fails.push({ doc, sdk: key, ref: f })
      }
    }
  }
  return { contracts, total: count, fails }
}

/** Track C — zh (root) and en docs are in structural parity. */
function checkParity(docs) {
  const isEn = d => d.startsWith("docs/en/")
  const rel = d => (isEn(d) ? d.slice("docs/en/".length) : d.slice("docs/".length))
  const zh = new Set()
  const en = new Set()
  for (const d of docs) {
    const r = rel(d)
    if (SKIP_FILES.has(r)) continue
    ;(isEn(d) ? en : zh).add(r)
  }
  const missingEn = [...zh].filter(r => !en.has(r)).sort()
  const orphanEn = [...en].filter(r => !zh.has(r)).sort()
  return { zh: zh.size, en: en.size, missingEn, orphanEn }
}

// ---- run ----
const docs = listDocs()
const manifest = buildManifest(ROOT)
const paths = checkPaths(docs)
const symbols = checkSymbols(docs, manifest)
const parity = checkParity(docs)

const failed =
  paths.fails.length + symbols.fails.length + parity.missingEn.length + parity.orphanEn.length

if (JSON_OUT) {
  console.log(JSON.stringify({ paths, symbols, parity, failed }, null, 2))
  process.exit(failed ? 1 : 0)
}

const ok = "✓"
const bad = "✗"
// A
if (paths.fails.length === 0) console.log(`${ok} paths    ${paths.total - paths.fails.length}/${paths.total} referenced source files exist`)
else {
  console.error(`${bad} paths    ${paths.fails.length}/${paths.total} references missing:`)
  for (const f of paths.fails) console.error(`    ${f.doc}:${f.line}  ${f.ref}`)
}
// B
if (symbols.fails.length === 0) console.log(`${ok} symbols  ${symbols.total}/${symbols.total} code_refs resolve (${symbols.contracts} contract docs)`)
else {
  console.error(`${bad} symbols  ${symbols.fails.length}/${symbols.total} unresolved:`)
  for (const f of symbols.fails) console.error(`    ${f.doc}  ${f.sdk}: ${f.ref}  → not in live manifest`)
}
// C
if (parity.missingEn.length === 0 && parity.orphanEn.length === 0)
  console.log(`${ok} parity   ${parity.zh}/${parity.zh} zh docs have en counterparts`)
else {
  console.error(`${bad} parity   zh=${parity.zh} en=${parity.en}`)
  for (const r of parity.missingEn) console.error(`    missing en:  docs/en/${r}`)
  for (const r of parity.orphanEn) console.error(`    orphan en:   docs/en/${r} (no zh source)`)
}

if (failed > 0) {
  console.error(`\nFAIL (${failed} drift${failed > 1 ? "s" : ""})`)
  process.exit(1)
}
console.log("\nPASS — docs narrative is grounded in current code")
