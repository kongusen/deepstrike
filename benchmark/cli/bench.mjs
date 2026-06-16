#!/usr/bin/env node
/**
 * bench — drive a BenchScenario × N variants, write MetricSets, optionally diff.
 *
 * Usage:
 *   bench <scenario-id> [--variants off,on] [--provider deepseek]
 *                       [--tasks 4] [--output .runs/<stamp>] [--compare] [--json]
 *   bench list                                                 # list scenarios
 *   bench --help
 *
 * The first variant in `--variants` is the baseline. Subsequent variants are each diffed against
 * it when `--compare` is set.
 */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { loadEnvFile, redact } from "../utils/env.mjs"
import { repoRoot, nodeRoot, resolveProvider, PROVIDER_IDS } from "../utils/sdk.mjs"
import { findScenario, listScenarios } from "../scenarios/index.mjs"
import { runBench } from "../core/runner.mjs"
import { diff } from "../core/diff.mjs"
import { renderDiff } from "../core/render.mjs"

const __dir = path.dirname(fileURLToPath(import.meta.url))
const benchRoot = path.resolve(__dir, "..")

loadEnvFile(path.join(repoRoot, ".env"))
loadEnvFile(path.join(nodeRoot, ".env"))

const rawArgs = process.argv.slice(2)
const flags = parseArgs(rawArgs)

if (flags.help) {
  printUsage(process.stdout)
  process.exit(0)
}
if (flags._.length === 0) {
  printUsage(process.stderr)
  process.exit(1)
}

if (flags._[0] === "list") {
  printList()
  process.exit(0)
}

const scenarioId = flags._[0]
const scenario = findScenario(scenarioId)
if (!scenario) {
  console.error(`Unknown scenario: ${scenarioId}`)
  console.error(`Available: ${listScenarios().map(s => s.id).join(", ")}`)
  process.exit(1)
}

const variantIds = flags.variants
  ? String(flags.variants).split(",").map(s => s.trim()).filter(Boolean)
  : (scenario.variantOrder ?? Object.keys(scenario.variants))

for (const v of variantIds) {
  if (!scenario.variants[v]) {
    console.error(`scenario ${scenarioId}: unknown variant "${v}". Known: ${Object.keys(scenario.variants).join(", ")}`)
    process.exit(1)
  }
}

const mode = String(flags.mode ?? "live").toLowerCase()
if (mode !== "live" && mode !== "replay") {
  console.error(`Unknown --mode "${mode}". Must be "live" or "replay".`)
  process.exit(1)
}
const fixtureRoot = typeof flags.fixture === "string" ? path.resolve(flags.fixture) : undefined
const fixtureFromVariant = typeof flags["fixture-from"] === "string" ? flags["fixture-from"] : undefined
if (mode === "replay" && !fixtureRoot) {
  console.error(`--mode replay requires --fixture <run-dir> (path to a prior run's output directory)`)
  process.exit(1)
}

let providerDesc
if (mode === "replay") {
  // No API key needed under replay — synthesise a metadata-only descriptor.
  // Provider/model come from --provider (for pricing lookup + MetricSet meta) or default to "replay".
  const fallbackPM = readFixtureProviderModel(fixtureRoot, scenarioId, fixtureFromVariant ?? variantIds[0])
  providerDesc = {
    provider: typeof flags.provider === "string" ? flags.provider : (fallbackPM?.provider ?? "replay"),
    model: typeof flags.model === "string" ? flags.model : (fallbackPM?.model ?? "replay"),
    apiKey: "(replay)",
  }
} else {
  try {
    providerDesc = resolveProvider(flags.provider)
  } catch (e) {
    console.error(redact(e.message))
    console.error(`Available providers: ${PROVIDER_IDS.join(", ")}`)
    process.exit(1)
  }
}

const stamp = new Date().toISOString().replace(/[-:.TZ]/g, "").slice(0, 14)
const runRoot = flags.output
  ? path.resolve(flags.output)
  : path.join(benchRoot, ".runs", `${scenarioId}-${stamp}`)
mkdirSync(runRoot, { recursive: true })

const pricing = loadPricing()
const maxTasks = flags.tasks ? parseInt(String(flags.tasks), 10) : undefined

console.log(JSON.stringify({
  scenario: scenarioId,
  mode,
  variants: variantIds,
  provider: providerDesc.provider,
  model: providerDesc.model,
  tasks: maxTasks ?? scenario.tasks.length,
  ...(mode === "replay" ? {
    fixtureRoot: path.relative(repoRoot, fixtureRoot),
    fixtureFromVariant: fixtureFromVariant ?? "(per-variant)",
  } : {}),
  runRoot: path.relative(repoRoot, runRoot),
}, null, 2))

const results = []
for (const variantId of variantIds) {
  console.log(`\n══ variant: ${variantId} ──────────────────────────────`)
  console.log(`   ${scenario.variants[variantId].description}`)
  try {
    const result = await runBench({
      scenario,
      variantId,
      providerDesc,
      runRoot,
      pricing,
      maxTasks,
      mode,
      ...(mode === "replay" ? { fixtureRoot, ...(fixtureFromVariant ? { fixtureFromVariant } : {}) } : {}),
      onEvent: (taskId, evt) => logEvent(taskId, evt),
    })
    results.push({ variantId, result })
    summariseRun(result)
  } catch (e) {
    console.error(`  [run-error] ${redact(String(e.message ?? e))}`)
    results.push({ variantId, error: String(e.message ?? e) })
  }
}

if (flags.compare && results.length >= 2) {
  const baseline = results.find(r => r.result)
  if (!baseline) {
    console.error("\n[compare] no successful baseline run — skipping diff")
  } else {
    for (const r of results) {
      if (r === baseline || !r.result) continue
      const diffResult = diff(baseline.result.metricSet, r.result.metricSet)
      if (flags.json) {
        console.log(JSON.stringify(diffResult, null, 2))
      } else {
        console.log("\n" + renderDiff(diffResult))
      }
      writeFileSync(
        path.join(runRoot, `diff.${baseline.variantId}-vs-${r.variantId}.json`),
        JSON.stringify(diffResult, null, 2),
      )
    }
  }
}

console.log(`\nDone. Outputs → ${path.relative(repoRoot, runRoot)}`)

// ── helpers ─────────────────────────────────────────────────────────────────

function parseArgs(args) {
  const out = { _: [] }
  for (let i = 0; i < args.length; i++) {
    const a = args[i]
    if (a === "--help" || a === "-h") { out.help = true; continue }
    if (a.startsWith("--")) {
      const key = a.slice(2)
      const next = args[i + 1]
      if (!next || next.startsWith("--")) { out[key] = true }
      else { out[key] = next; i++ }
      continue
    }
    out._.push(a)
  }
  return out
}

function printUsage(stream = process.stderr) {
  stream.write(`Usage:
  node benchmark/cli/bench.mjs <scenario-id> [options]
  node benchmark/cli/bench.mjs list
  node benchmark/cli/bench.mjs --help

Options:
  --variants <a,b,...>   Variants to run. First is baseline. Default: scenario.variantOrder
  --mode <live|replay>   live = real LLM call; replay = SDK ReplayProvider against a fixture. Default: live
  --provider <id>        Provider id (${PROVIDER_IDS.join(" | ")}). Default: env LLM_PROVIDER (live mode);
                         metadata-only / pricing lookup (replay mode)
  --fixture <dir>        REQUIRED with --mode replay: path to a prior run dir holding events.json
  --fixture-from <vid>   Pin all replay variants to read THIS variant's fixture (cross-variant pin
                         for cost-Δ measurement). Default: each variant reads its own fixture
  --tasks <n>            Limit task count (default: scenario default)
  --output <dir>         Output dir (default: benchmark/.runs/<scenario>-<stamp>)
  --compare              After running, diff baseline vs each other variant
  --json                 Emit JSON instead of human table (with --compare)

Examples:
  # Live A/B:
  bench gating-dwell --variants off,on --provider deepseek --compare

  # Replay A (sanity): both variants replay their own prior fixtures
  bench gating-dwell --variants off,on --mode replay --fixture .runs/gating-dwell-<stamp> --compare

  # Replay B (cross-variant cost Δ): pin behavior to off's recording, run both variants against it
  bench gating-dwell --variants off,on --mode replay --fixture .runs/gating-dwell-<stamp> \\
                     --fixture-from off --compare
`)
}

function printList() {
  const items = listScenarios()
  console.log("Scenarios:\n")
  for (const s of items) {
    console.log(`  ${s.id}`)
    console.log(`    ${s.description}`)
    console.log(`    variants: ${s.variants.join(", ")}\n`)
  }
}

function loadPricing() {
  try {
    return JSON.parse(readFileSync(path.join(benchRoot, "pricing", "pricing.json"), "utf8"))
  } catch { return undefined }
}

/**
 * For replay mode without an explicit --provider: read provider/model off the fixture's
 * metricset.json so MetricSet metadata and pricing lookup reflect what the fixture used.
 */
function readFixtureProviderModel(fixtureRoot, scenarioId, variantId) {
  if (!fixtureRoot) return undefined
  const metricsetPath = path.join(fixtureRoot, `${scenarioId}.${variantId}`, "metricset.json")
  try {
    const m = JSON.parse(readFileSync(metricsetPath, "utf8"))
    return { provider: m.meta?.provider, model: m.meta?.model }
  } catch { return undefined }
}

function logEvent(taskId, evt) {
  if (evt.type === "tool_call") {
    process.stdout.write(`    [${taskId}] tool ${evt.name}\n`)
  } else if (evt.type === "error") {
    process.stdout.write(`    [${taskId}] error ${redact(String(evt.message ?? ""))}\n`)
  } else if (evt.type === "done") {
    // SDK's `done` event omits turnsUsed; the aggregator gets turn count from turnMetrics.length.
    process.stdout.write(`    [${taskId}] done status=${evt.status ?? "?"}\n`)
  }
}

function summariseRun(result) {
  const m = result.metricSet
  const tpt = m.cost.tokensPerTurn
  const exp = m.mechanism.avgToolsExposed
  const cache = m.cost.cacheHitRate
  const $ = m.cost.dollars
  console.log(
    `   summary: tokens/turn=${fmt(tpt)}  toolsExposed=${fmt(exp)}  cacheHit=${fmt(cache)}  $=${fmt($)}` +
    `\n   → ${path.relative(repoRoot, result.metricSetPath)}`,
  )
}

function fmt(mv) {
  if (!mv) return "n/a"
  const v = scale(mv.value)
  return mv.stdev && (mv.samples ?? 1) > 1 ? `${v}±${scale(mv.stdev)}` : v
}
function scale(n) {
  const a = Math.abs(n)
  if (a === 0) return "0"
  if (a >= 1) return String(Math.round(n * 100) / 100)
  if (a >= 0.01) return String(Math.round(n * 10000) / 10000)
  return n.toFixed(6).replace(/\.?0+$/, "")
}
