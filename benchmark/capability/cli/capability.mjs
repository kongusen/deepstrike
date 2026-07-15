#!/usr/bin/env node
/**
 * capability — external capability eval CLI (BFCL / GAIA / WebArena).
 *
 * Usage:
 *   capability list
 *   capability <suite> [--provider deepseek] [--limit N] [--dataset path] [--output dir]
 *
 * Default provider: deepseek (reads DEEPSEEK_API_KEY from repo .env).
 */

import { mkdirSync } from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { loadEnvFile, redact } from "../../utils/env.mjs"
import { repoRoot, nodeRoot, resolveProvider, PROVIDER_IDS } from "../../utils/sdk.mjs"
import { getAdapter, listAdapters, adapterIds } from "../adapters/index.mjs"
import { runCapability } from "../core/runner.mjs"
import { renderReportSummary } from "../core/report.mjs"

const __dir = path.dirname(fileURLToPath(import.meta.url))
const benchRoot = path.resolve(__dir, "../..")
const capRoot = path.resolve(__dir, "..")

loadEnvFile(path.join(repoRoot, ".env"))
loadEnvFile(path.join(nodeRoot, ".env"))
loadEnvFile(path.join(benchRoot, ".env"))

const rawArgs = process.argv.slice(2)
const flags = parseArgs(rawArgs)

if (flags.help || flags._.length === 0) {
  printUsage(flags.help ? process.stdout : process.stderr)
  process.exit(flags.help ? 0 : 1)
}

if (flags._[0] === "list") {
  console.log("Capability suites:\n")
  for (const a of listAdapters()) {
    console.log(`  ${a.id}`)
    console.log(`    ${a.description}\n`)
  }
  process.exit(0)
}

const suiteId = String(flags._[0]).toLowerCase()
const adapter = getAdapter(suiteId)
if (!adapter) {
  console.error(`Unknown suite: ${suiteId}`)
  console.error(`Available: ${adapterIds().join(", ")}`)
  process.exit(1)
}

if (suiteId === "webarena") {
  console.error("webarena is a stub — see benchmark/capability/adapters/webarena/README.md")
  process.exit(2)
}

const providerId = String(flags.provider ?? "deepseek")
let providerDesc
try {
  providerDesc = resolveProvider(providerId)
} catch (e) {
  console.error(redact(e?.message ? String(e.message) : String(e)))
  process.exit(1)
}

const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19)
const runRoot = path.resolve(
  String(flags.output ?? path.join(benchRoot, ".runs", `capability-${suiteId}-${stamp}`)),
)
mkdirSync(runRoot, { recursive: true })

const limit = flags.limit != null ? Number(flags.limit) : undefined
const dataset = flags.dataset ? String(flags.dataset) : undefined

console.log(`capability ${suiteId}`)
console.log(`  provider: ${providerDesc.provider} / ${providerDesc.model}`)
console.log(`  output:   ${runRoot}`)
if (limit != null) console.log(`  limit:    ${limit}`)
if (dataset) console.log(`  dataset:  ${dataset}`)
console.log("")

try {
  const { report, reportPath } = await runCapability({
    adapter,
    providerDesc,
    runRoot,
    limit,
    dataset,
    onEvent: (taskId, evt) => {
      if (evt.type === "capability_grade") {
        const mark = evt.passed ? "PASS" : "FAIL"
        console.log(`  → ${taskId}: ${mark} (score=${Number(evt.score).toFixed(2)})${evt.reason ? ` ${evt.reason}` : ""}`)
      } else if (evt.type === "done") {
        console.log(`  … ${taskId}: done status=${evt.status}`)
      } else if (flags.verbose && evt.type === "tool_call") {
        console.log(`  … ${taskId}: tool ${evt.name}`)
      }
    },
  })
  console.log("")
  renderReportSummary(report)
  console.log(`report: ${reportPath}`)
  process.exit(report.passedCount === report.taskCount ? 0 : 0) // smoke never fails CI by accuracy alone
} catch (e) {
  console.error(redact(e?.message ? String(e.message) : String(e)))
  if (flags.verbose && e?.stack) console.error(e.stack)
  process.exit(1)
}

/** @param {string[]} argv */
function parseArgs(argv) {
  /** @type {Record<string, any> & { _: string[] }} */
  const out = { _: [] }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === "--help" || a === "-h") out.help = true
    else if (a === "--verbose" || a === "-v") out.verbose = true
    else if (a === "--provider") out.provider = argv[++i]
    else if (a.startsWith("--provider=")) out.provider = a.slice("--provider=".length)
    else if (a === "--limit") out.limit = argv[++i]
    else if (a.startsWith("--limit=")) out.limit = a.slice("--limit=".length)
    else if (a === "--dataset") out.dataset = argv[++i]
    else if (a.startsWith("--dataset=")) out.dataset = a.slice("--dataset=".length)
    else if (a === "--output") out.output = argv[++i]
    else if (a.startsWith("--output=")) out.output = a.slice("--output=".length)
    else if (a.startsWith("-")) {
      console.error(`Unknown flag: ${a}`)
      process.exit(1)
    } else out._.push(a)
  }
  return out
}

/** @param {NodeJS.WritableStream} out */
function printUsage(out) {
  out.write(`Usage:
  capability list
  capability <suite> [options]

Suites: ${adapterIds().join(", ")}

Options:
  --provider <id>   LLM provider (default: deepseek). Known: ${PROVIDER_IDS.join(", ")}
  --limit <N>       Only first N tasks (smoke)
  --dataset <path>  External JSON dataset (optional; else built-in smoke-tasks)
  --output <dir>    Run output directory (default: benchmark/.runs/capability-<suite>-<stamp>)
  --verbose         Log tool calls
  --help            Show this help

Examples:
  node capability/cli/capability.mjs list
  node capability/cli/capability.mjs bfcl --provider deepseek --limit 8
  node capability/cli/capability.mjs gaia --provider deepseek --limit 5
`)
}
