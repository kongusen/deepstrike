#!/usr/bin/env node
/**
 * Session-event registry generator. `contracts/session-events.json` is the single authority for
 * the cross-SDK session-event vocabulary: every kind, its top-level fields (required/optional),
 * which SDKs carry it in their typed vocabulary, and one full sample in the shared wire shape.
 *
 * Everything else is derived from it:
 * - the SPC-017 fixture `session-event-vocabulary.json` (per-SDK projection is runner-owned),
 * - `node/src/runtime/session-event-registry.generated.ts`,
 * - `wasm/src/runtime/session-event-registry.generated.ts`,
 * - `python/deepstrike/runtime/_session_event_registry.py`.
 * Rust reads the registry directly from its core tests (`include_str!`).
 *
 * Usage: node scripts/gen-session-event-registry.mjs [--check]
 *   --check  fail (exit 1) when a generated file is stale instead of rewriting it.
 */
import { readFileSync, writeFileSync, existsSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const registryPath = join(root, "contracts", "session-events.json")
const check = process.argv.includes("--check")

const registry = JSON.parse(readFileSync(registryPath, "utf8"))
validate(registry)

const outputs = [
  [join(root, "tests", "fixtures", "sdk-conformance", "canonical", "session-event-vocabulary.json"), fixture(registry)],
  [join(root, "node", "src", "runtime", "session-event-registry.generated.ts"), typescript(registry, "node")],
  [join(root, "wasm", "src", "runtime", "session-event-registry.generated.ts"), typescript(registry, "wasm")],
  [join(root, "python", "deepstrike", "runtime", "_session_event_registry.py"), python(registry, "python")],
]

let stale = 0
for (const [path, content] of outputs) {
  const current = existsSync(path) ? readFileSync(path, "utf8") : undefined
  if (current === content) continue
  if (check) {
    stale += 1
    console.error(`stale: ${path.slice(root.length + 1)}`)
    continue
  }
  writeFileSync(path, content)
  console.log(`wrote ${path.slice(root.length + 1)}`)
}
if (stale) {
  console.error(`\n${stale} generated file(s) out of date with contracts/session-events.json — run node scripts/gen-session-event-registry.mjs`)
  process.exit(1)
}
if (check) console.log(`Session-event registry generated files are current (${registry.events.length} kinds)`)

function validate(value) {
  const fail = message => {
    console.error(`contracts/session-events.json: ${message}`)
    process.exit(1)
  }
  if (value.version !== 1) fail("unsupported registry version")
  if (!Array.isArray(value.sdks) || !value.sdks.length) fail("/sdks must list the SDKs")
  const seen = new Set()
  for (const [index, event] of value.events.entries()) {
    const at = `/events/${index} (${event.kind})`
    if (!/^[a-z][a-z0-9_]*$/.test(event.kind ?? "")) fail(`${at}: kind must be snake_case`)
    if (seen.has(event.kind)) fail(`${at}: duplicate kind`)
    seen.add(event.kind)
    if (typeof event.group !== "string" || !event.group) fail(`${at}: group is required`)
    if (!Array.isArray(event.sdks) || !event.sdks.length) fail(`${at}: sdks must be non-empty`)
    for (const sdk of event.sdks) if (!value.sdks.includes(sdk)) fail(`${at}: unknown sdk ${sdk}`)
    for (const key of ["required", "optional"]) {
      if (!Array.isArray(event[key])) fail(`${at}: ${key} must be an array`)
      if ([...event[key]].sort().join() !== event[key].join()) fail(`${at}: ${key} must be sorted`)
      if (event[key].includes("kind")) fail(`${at}: kind is implicit, do not list it`)
    }
    const overlap = event.required.filter(field => event.optional.includes(field))
    if (overlap.length) fail(`${at}: fields both required and optional: ${overlap.join(", ")}`)
    if (!event.sample || event.sample.kind !== event.kind) fail(`${at}: sample must be an event of this kind`)
    const declared = [...event.required, ...event.optional].sort()
    const sampled = Object.keys(event.sample).filter(key => key !== "kind").sort()
    if (declared.join() !== sampled.join()) {
      fail(`${at}: sample must carry every declared field exactly (declared ${declared.join(",")}; sample ${sampled.join(",")})`)
    }
  }
}

function fixture(value) {
  const kinds = {}
  for (const event of [...value.events].sort((a, b) => a.kind.localeCompare(b.kind))) {
    kinds[event.kind] = { required: event.required, optional: event.optional, sdks: event.sdks }
  }
  const samples = {}
  for (const event of value.events) samples[event.kind] = event.sample
  return `${JSON.stringify({
    id: "session-event-vocabulary",
    domain: "session_event_vocabulary",
    input: { samples },
    expected: { canonical: { kinds } },
  }, null, 2)}\n`
}

function forSdk(value, sdk) {
  return value.events.filter(event => event.sdks.includes(sdk))
}

function typescript(value, sdk) {
  const events = forSdk(value, sdk)
  const list = items => `[${items.map(item => JSON.stringify(item)).join(", ")}]`
  return [
    "// GENERATED by scripts/gen-session-event-registry.mjs from contracts/session-events.json.",
    "// Do not edit: change the registry and regenerate. `session-log.ts` asserts at compile time",
    "// that the SessionEvent union matches these kinds and fields exactly.",
    "",
    "export const SESSION_EVENT_REGISTRY = {",
    ...events.map(event =>
      `  ${event.kind}: { required: ${list(event.required)}, optional: ${list(event.optional)} },`),
    "} as const",
    "",
    "/** Registered kinds in registry order. */",
    "export const SESSION_EVENT_KINDS = [",
    ...events.map(event => `  ${JSON.stringify(event.kind)},`),
    "] as const",
    "",
  ].join("\n")
}

function python(value, sdk) {
  const events = forSdk(value, sdk)
  const tuple = items => {
    const body = items.map(item => JSON.stringify(item)).join(", ")
    return items.length === 1 ? `(${body},)` : `(${body})`
  }
  return [
    "# GENERATED by scripts/gen-session-event-registry.mjs from contracts/session-events.json.",
    "# Do not edit: change the registry and regenerate. `session_log.py` asserts at import time",
    "# that the SessionEvent TypedDicts match these kinds and fields exactly.",
    "",
    "from __future__ import annotations",
    "",
    "SESSION_EVENT_REGISTRY: dict[str, dict[str, tuple[str, ...]]] = {",
    ...events.map(event =>
      `    ${JSON.stringify(event.kind)}: {"required": ${tuple(event.required)}, "optional": ${tuple(event.optional)}},`),
    "}",
    "",
    "SESSION_EVENT_KINDS: tuple[str, ...] = tuple(SESSION_EVENT_REGISTRY)",
    "",
  ].join("\n")
}
