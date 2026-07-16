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

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { loadEnvFile, redact } from "../utils/env.mjs"
import { repoRoot, nodeRoot, resolveProvider, PROVIDER_IDS } from "../utils/sdk.mjs"
import { findScenario, listScenarios } from "../scenarios/index.mjs"
import { runBench } from "../core/runner.mjs"
import { diff } from "../core/diff.mjs"
import { renderDiff } from "../core/render.mjs"
import { saveGolden, loadGolden, checkGolden, goldenPath, keyFromMetricSet, renderGoldenCheck } from "../core/golden.mjs"

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

const stubDriver = scenario.requiresProvider === false || typeof scenario.driveTask === "function"

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
} else if (stubDriver) {
  // Kernel/stub drivers (e.g. orchestration F1–F3) need no LLM key.
  providerDesc = {
    provider: typeof flags.provider === "string" ? flags.provider : "stub",
    model: typeof flags.model === "string" ? flags.model : "stub",
    apiKey: "(stub)",
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

// BM3 judge: default on for live mode, forced off for replay (no model behavior to evaluate).
// Stub/kernel drivers default judge off — there is no model output to score.
const judgeRequested = flags["no-judge"] === true
  ? false
  : flags.judge === true
    ? true
    : mode === "live" && !stubDriver // implicit default
let judgeProviderDesc
if (judgeRequested && mode === "replay") {
  console.error(`[bench] --judge ignored under --mode replay (replayed responses are deterministic; ` +
    `judge would produce identical verdicts across variants).`)
}
if (judgeRequested && stubDriver) {
  console.error(`[bench] --judge ignored for stub/kernel scenario ${scenarioId} (no LLM submission).`)
}
if (judgeRequested && mode === "live" && !stubDriver) {
  const judgeProviderId = typeof flags["judge-provider"] === "string"
    ? flags["judge-provider"]
    : providerDesc.provider
  try {
    judgeProviderDesc = resolveProvider(judgeProviderId)
    if (typeof flags["judge-model"] === "string") {
      judgeProviderDesc = { ...judgeProviderDesc, model: flags["judge-model"] }
    }
  } catch (e) {
    console.error(`[bench] --judge requested but judge provider resolution failed: ${redact(e.message)}`)
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
const samples = flags.samples ? Math.max(1, parseInt(String(flags.samples), 10) || 1) : 1

console.log(JSON.stringify({
  scenario: scenarioId,
  mode,
  variants: variantIds,
  provider: providerDesc.provider,
  model: providerDesc.model,
  tasks: maxTasks ?? scenario.tasks.length,
  samples,
  ...(judgeProviderDesc
    ? { judge: { provider: judgeProviderDesc.provider, model: judgeProviderDesc.model } }
    : { judge: "off" }),
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
      samples,
      mode,
      ...(mode === "replay" ? { fixtureRoot, ...(fixtureFromVariant ? { fixtureFromVariant } : {}) } : {}),
      ...(judgeProviderDesc ? { judge: { providerDesc: judgeProviderDesc } } : {}),
      onEvent: (taskId, evt) => logEvent(taskId, evt),
    })
    results.push({ variantId, result })
    summariseRun(result)
  } catch (e) {
    console.error(`  [run-error] ${redact(String(e.message ?? e))}`)
    results.push({ variantId, error: String(e.message ?? e) })
  }
}

// BM4: golden baseline save / check / update.
const baselinesDir = typeof flags["baseline-dir"] === "string"
  ? path.resolve(flags["baseline-dir"])
  : path.join(benchRoot, "baselines")
const baselineSave = flags["baseline-save"] === true
const baselineCheck = flags["baseline-check"] === true
const baselineUpdate = flags["baseline-update"] === true
let baselineFailures = 0

if (baselineSave || baselineCheck || baselineUpdate) {
  console.log(`\n══ baselines · ${path.relative(repoRoot, baselinesDir)} ──────────────────────────`)
}
for (const r of results) {
  if (!r.result) continue
  const set = r.result.metricSet
  if (baselineSave) {
    const written = saveGolden({ metricSet: set, baselinesDir })
    console.log(`  ✓ saved golden: ${path.relative(repoRoot, written)}`)
  }
  if (baselineCheck || baselineUpdate) {
    const p = goldenPath(baselinesDir, keyFromMetricSet(set))
    if (!fsExists(p)) {
      if (baselineUpdate) {
        const written = saveGolden({ metricSet: set, baselinesDir })
        console.log(`  ℹ  no golden — initialised: ${path.relative(repoRoot, written)}`)
      } else {
        console.error(`  ✗ no golden at ${path.relative(repoRoot, p)} (run with --baseline-save to create it)`)
        baselineFailures++
      }
      continue
    }
    const golden = loadGolden(p)
    const checkResult = checkGolden(set, golden)
    console.log(renderGoldenCheck(checkResult))
    writeFileSync(
      path.join(runRoot, `golden-check.${r.variantId}.json`),
      JSON.stringify(checkResult, null, 2),
    )
    if (!checkResult.passed) {
      if (baselineUpdate) {
        const written = saveGolden({ metricSet: set, baselinesDir })
        console.log(`  ↻ updated golden after check: ${path.relative(repoRoot, written)}`)
      } else {
        baselineFailures++
      }
    }
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
if (baselineFailures > 0) {
  console.error(`\n[bench] ${baselineFailures} variant(s) failed --baseline-check (exit 2). Run with --baseline-update to refresh.`)
  process.exit(2)
}

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
  --judge / --no-judge   Run SDK.judge() over each session's final output to populate
                         quality.successRate + quality.overallScore. Default: ON in live mode,
                         OFF in replay mode (replayed responses are deterministic across variants).
  --judge-provider <id>  Provider for the judge LLM. Default: same as --provider.
  --judge-model <model>  Model override for the judge LLM. Default: same as --provider's model.
  --tasks <n>            Limit task count (default: scenario default)
  --samples <n>          Repeat the full task list N times per variant; stdev tightens (default: 1)
  --output <dir>         Output dir (default: benchmark/.runs/<scenario>-<stamp>)
  --compare              After running, diff baseline vs each other variant
  --json                 Emit JSON instead of human table (with --compare)
  --baseline-save        After each variant runs, save its MetricSet as a golden in --baseline-dir
  --baseline-check       After each variant runs, compare against golden; exit 2 on any failure
  --baseline-update      Like --baseline-check, but refresh the golden on failure (CI escape hatch)
  --baseline-dir <dir>   Where goldens live (default: benchmark/baselines/)

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

function fsExists(p) {
  try { return existsSync(p) } catch { return false }
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
  } else if (evt.type === "judge") {
    const pass = evt.passed ? "PASS" : "FAIL"
    const score = typeof evt.overallScore === "number" ? evt.overallScore.toFixed(2) : "n/a"
    process.stdout.write(`    [${taskId}] judge ${pass} score=${score}\n`)
  } else if (evt.type === "judge_error") {
    process.stdout.write(`    [${taskId}] judge_error ${redact(String(evt.message ?? ""))}\n`)
  }
}

function summariseRun(result) {
  const m = result.metricSet
  const tpt = m.cost.tokensPerTurn
  const exp = m.mechanism.avgToolsExposed
  const cache = m.cost.cacheHitRate
  const $ = m.cost.dollars
  const success = m.quality?.successRate
  const score = m.quality?.overallScore
  const qualityBit = success || score
    ? `  success=${fmt(success)}  score=${fmt(score)}`
    : ""
  console.log(
    `   summary: tokens/turn=${fmt(tpt)}  toolsExposed=${fmt(exp)}  cacheHit=${fmt(cache)}  $=${fmt($)}${qualityBit}` +
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
