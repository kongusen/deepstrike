#!/usr/bin/env node
/**
 * bench diff — read two MetricSet JSONs and print the Δ table.
 *
 * Usage:
 *   node benchmark/cli/diff.mjs <baseline.json> <variant.json>
 *                               [--json]
 *
 * Both files must use the canonical MetricSet shape.
 */

import { readFileSync } from "node:fs"
import { resolve } from "node:path"

import { assertMetricSet } from "../core/metrics.mjs"
import { diff } from "../core/diff.mjs"
import { renderDiff } from "../core/render.mjs"

const args = process.argv.slice(2)
const positional = args.filter(a => !a.startsWith("--"))

if (positional.length < 2 || args.includes("--help") || args.includes("-h")) {
  usage()
  process.exit(positional.length < 2 ? 1 : 0)
}

const baselinePath = resolve(positional[0])
const variantPath = resolve(positional[1])

const baseline = loadMetricSet(baselinePath)
const variant = loadMetricSet(variantPath)

const result = diff(baseline, variant)

if (args.includes("--json")) {
  process.stdout.write(JSON.stringify(result, null, 2) + "\n")
} else {
  process.stdout.write(renderDiff(result) + "\n")
}

// ── helpers ─────────────────────────────────────────────────────────────────

function loadMetricSet(path) {
  const raw = JSON.parse(readFileSync(path, "utf8"))
  return assertMetricSet(raw)
}

function usage() {
  process.stderr.write(`Usage:
  node benchmark/cli/diff.mjs <baseline.json> <variant.json>
                              [--json]

Examples:
  # Diff two MetricSet JSONs:
  node benchmark/cli/diff.mjs runs/off.json runs/on.json
`)
}
