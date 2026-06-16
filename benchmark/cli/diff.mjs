#!/usr/bin/env node
/**
 * bench diff — read two run JSONs (MetricSet or legacy dwell-report) and print the Δ table.
 *
 * Usage:
 *   node benchmark/cli/diff.mjs <baseline.json> <variant.json>
 *                               [--baseline-id <name>] [--variant-id <name>]
 *                               [--scenario <id>] [--no-pricing] [--json]
 *
 * Auto-detects shape per file:
 *   - has `schema: "deepstrike-bench/v0"` → MetricSet, used as-is
 *   - has `report.dwell` + `metrics[]`     → dwell-report.json, run through adapters/dwell-report
 *
 * When using a dwell-report, you SHOULD pass --baseline-id / --variant-id so the diff header reflects
 * the variant being compared (e.g. "off" vs "on"). Defaults: "baseline" / "variant".
 */

import { readFileSync } from "node:fs"
import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"

import { assertMetricSet } from "../core/metrics.mjs"
import { diff } from "../core/diff.mjs"
import { renderDiff } from "../core/render.mjs"
import { dwellReportToMetricSet } from "../adapters/dwell-report.mjs"

const __dir = dirname(fileURLToPath(import.meta.url))
const benchRoot = resolve(__dir, "..")

const args = process.argv.slice(2)
const positional = args.filter(a => !a.startsWith("--"))
const flag = (name, def) => {
  const i = args.indexOf(`--${name}`)
  if (i < 0) return def
  const next = args[i + 1]
  return next && !next.startsWith("--") ? next : true
}

if (positional.length < 2 || args.includes("--help") || args.includes("-h")) {
  usage()
  process.exit(positional.length < 2 ? 1 : 0)
}

const baselinePath = resolve(positional[0])
const variantPath = resolve(positional[1])

const pricingDisabled = args.includes("--no-pricing")
const pricing = pricingDisabled ? undefined : loadPricing()

const scenarioId = typeof flag("scenario") === "string" ? flag("scenario") : undefined
const baselineId = typeof flag("baseline-id") === "string" ? flag("baseline-id") : "baseline"
const variantId = typeof flag("variant-id") === "string" ? flag("variant-id") : "variant"

const baseline = loadAsMetricSet(baselinePath, { variantId: baselineId, scenarioId, pricing })
const variant = loadAsMetricSet(variantPath, { variantId: variantId, scenarioId, pricing })

const result = diff(baseline, variant)

if (args.includes("--json")) {
  process.stdout.write(JSON.stringify(result, null, 2) + "\n")
} else {
  process.stdout.write(renderDiff(result) + "\n")
}

// ── helpers ─────────────────────────────────────────────────────────────────

function loadAsMetricSet(path, opts) {
  const raw = JSON.parse(readFileSync(path, "utf8"))
  if (raw?.schema === "deepstrike-bench/v0") return assertMetricSet(raw)
  if (raw?.report && Array.isArray(raw?.metrics)) {
    return dwellReportToMetricSet(raw, opts)
  }
  throw new Error(`${path}: unrecognised shape (need MetricSet or dwell-report)`)
}

function loadPricing() {
  try {
    const path = resolve(benchRoot, "pricing", "pricing.json")
    return JSON.parse(readFileSync(path, "utf8"))
  } catch {
    return undefined
  }
}

function usage() {
  process.stderr.write(`Usage:
  node benchmark/cli/diff.mjs <baseline.json> <variant.json>
                              [--baseline-id <name>] [--variant-id <name>]
                              [--scenario <id>] [--no-pricing] [--json]

Examples:
  # Diff two MetricSet JSONs:
  node benchmark/cli/diff.mjs runs/off.json runs/on.json

  # Diff two dwell-report.json files (auto-imported):
  node benchmark/cli/diff.mjs \\
    node/examples/.gating-runs/run-A/dwell-report.json \\
    node/examples/.gating-runs/run-B/dwell-report.json \\
    --baseline-id off --variant-id on --scenario gating-dwell
`)
}
