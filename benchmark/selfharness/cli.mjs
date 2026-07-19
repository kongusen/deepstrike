#!/usr/bin/env node
/**
 * self-harness CLI (H3) — drive the propose→validate→promote loop from the shell.
 *
 * Usage:
 *   node benchmark/selfharness/cli.mjs --adapter <fixture|live|./path.mjs>
 *        --held-in a,b --held-out c,d --rounds T --k K --repeats R
 *        [--seed manifest.json] [--provider deepseek] [--lineage .harness-lab]
 *
 * The adapter module supplies tasks; `--held-in` / `--held-out` name the split by task id. The miner
 * and proposer are driven by a real provider `complete(prompt)` (resolved from env, matching the bench
 * CLI's provider conventions). No new dependencies — argv is parsed by hand.
 *
 * Built-in adapters:
 *   fixture  deterministic, zero-cost (adapters/fixture.mjs) — miner/proposer still need a provider
 *   live     real single-attempt runs + judge (adapters/live.mjs) — a small demo task set
 * A path adapter must export `createAdapter(ctx)` where ctx = { providerDesc, judgeProviderDesc }.
 */

import { existsSync, readFileSync } from "node:fs"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

import { loadEnvFile } from "../utils/env.mjs"
import { loadSdk, repoRoot, nodeRoot, resolveProvider, PROVIDER_IDS } from "../utils/sdk.mjs"
import { selfHarnessLoop } from "./loop.mjs"

const __dir = path.dirname(fileURLToPath(import.meta.url))

loadEnvFile(path.join(repoRoot, ".env"))
loadEnvFile(path.join(nodeRoot, ".env"))

const flags = parseArgs(process.argv.slice(2))

if (flags.help || flags.h) {
  printUsage()
  process.exit(0)
}

const adapterRef = str(flags.adapter)
if (!adapterRef) fail("--adapter <fixture|live|./module.mjs> is required")

const heldIn = list(flags["held-in"])
const heldOut = list(flags["held-out"])
if (heldIn.length === 0) fail("--held-in <ids> is required")
if (heldOut.length === 0) fail("--held-out <ids> is required")

const rounds = int(flags.rounds, 1)
const k = int(flags.k, 4)
const repeats = int(flags.repeats, 1)
const lineageDir = str(flags.lineage) ? path.resolve(str(flags.lineage)) : path.join(repoRoot, ".harness-lab")

// Resolve a provider for the miner/proposer `complete` (and, for the live adapter, the runs).
let providerDesc
try {
  providerDesc = resolveProvider(flags.provider)
} catch (e) {
  fail(`${e.message}\nAvailable providers: ${PROVIDER_IDS.join(", ")}`)
}

const sdk = await loadSdk()
const provider = sdk.createProvider({
  provider: providerDesc.provider,
  model: providerDesc.model,
  apiKey: providerDesc.apiKey,
  ...(providerDesc.baseURL ? { baseURL: providerDesc.baseURL } : {}),
  ...(providerDesc.endpoint ? { endpoint: providerDesc.endpoint } : {}),
  retry: { maxRetries: 2, baseDelay: 600 },
})

/** @param {string} prompt @returns {Promise<string>} */
async function complete(prompt) {
  const ctx = { systemText: "You are a careful harness engineer.", turns: [{ role: "user", content: prompt }] }
  let text = ""
  for await (const evt of provider.stream(ctx, [], undefined, undefined)) {
    if (evt.type === "text_delta") text += evt.delta ?? ""
  }
  return text
}

const adapterModule = await loadAdapterModule(adapterRef)
if (typeof adapterModule.createAdapter !== "function") {
  fail(`adapter module "${adapterRef}" does not export createAdapter(ctx)`)
}
const adapter = adapterModule.createAdapter({ providerDesc, judgeProviderDesc: providerDesc })

const seedManifest = str(flags.seed)
  ? JSON.parse(readFileSync(path.resolve(str(flags.seed)), "utf8"))
  : (typeof adapterModule.fixtureSeedManifest === "function"
    ? adapterModule.fixtureSeedManifest()
    : typeof adapterModule.seedManifest === "function"
      ? adapterModule.seedManifest()
      : fail("no --seed given and adapter exposes no default seedManifest()"))

console.log(JSON.stringify({
  adapter: adapter.id,
  provider: providerDesc.provider,
  model: providerDesc.model,
  heldIn,
  heldOut,
  rounds,
  k,
  repeats,
  lineage: path.relative(repoRoot, lineageDir),
}, null, 2))

const { finalManifest, trajectory } = await selfHarnessLoop({
  seedManifest,
  adapter,
  heldIn,
  heldOut,
  rounds,
  k,
  repeats,
  complete,
  lineageDir,
  log: msg => console.log(msg),
})

printTrajectory(trajectory)
console.log(`\nFinal harness digest: ${sdk.manifestDigest(finalManifest)}`)
console.log(`Lineage → ${path.relative(repoRoot, lineageDir)}`)

// ── helpers ─────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = { _: [] }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === "--help" || a === "-h") { out.help = true; continue }
    if (a.startsWith("--")) {
      const key = a.slice(2)
      const next = argv[i + 1]
      if (!next || next.startsWith("--")) out[key] = true
      else { out[key] = next; i++ }
      continue
    }
    out._.push(a)
  }
  return out
}

function str(v) {
  return typeof v === "string" ? v : ""
}
function list(v) {
  return str(v).split(",").map(s => s.trim()).filter(Boolean)
}
function int(v, dflt) {
  const n = parseInt(str(v), 10)
  return Number.isFinite(n) && n > 0 ? n : dflt
}

function fail(msg) {
  console.error(`[self-harness] ${msg}`)
  process.exit(1)
}

/** Resolve "fixture"/"live" to the bundled adapters, else import a path. */
async function loadAdapterModule(ref) {
  const builtin = { fixture: "./adapters/fixture.mjs", live: "./adapters/live.mjs" }
  const target = builtin[ref] ? path.join(__dir, builtin[ref]) : path.resolve(ref)
  if (!existsSync(target)) fail(`adapter module not found: ${target}`)
  return import(pathToFileURL(target).href)
}

function printTrajectory(trajectory) {
  console.log(`\n══ trajectory (${trajectory.length} rounds) ═══════════════════════════════`)
  console.log("round  baseIn/baseOut  proposals  accepted  promoted")
  for (const r of trajectory) {
    const accepted = r.decisions.filter(d => d.accepted).length
    console.log(
      `  ${String(r.round).padEnd(4)} ` +
      `${String(r.baseline.heldIn)}/${String(r.baseline.heldOut)}`.padEnd(15) +
      `${String(r.proposals.length)}`.padEnd(10) +
      `${String(accepted)}`.padEnd(9) +
      `${r.promotedDigest.slice(0, 12)}`,
    )
  }
}

function printUsage() {
  process.stdout.write(`Usage:
  node benchmark/selfharness/cli.mjs --adapter <fixture|live|./module.mjs> \\
       --held-in a,b --held-out c,d --rounds T --k K --repeats R \\
       [--seed manifest.json] [--provider ${PROVIDER_IDS.join("|")}] [--lineage .harness-lab]

The adapter module supplies tasks; --held-in / --held-out name the split by task id.
The miner + proposer are driven by the resolved provider's LLM.
`)
}
