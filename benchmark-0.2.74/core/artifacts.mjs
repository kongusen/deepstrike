import { mkdir, readFile, writeFile } from "node:fs/promises"
import { dirname, resolve } from "node:path"
import { diffMetrics } from "./metrics.mjs"

export function makeArtifact({ sdk, scenario, variant, result, elapsedMs }) {
  return {
    schemaVersion: 1,
    sdkVersion: sdk.version,
    scenario,
    variant,
    status: "passed",
    elapsedMs,
    metrics: result.metrics,
    evidence: result.evidence,
  }
}

export function makeFailureArtifact({ sdk, scenario, variant, error, elapsedMs }) {
  return {
    schemaVersion: 1,
    sdkVersion: sdk.version,
    scenario,
    variant,
    status: "failed",
    elapsedMs,
    error: { name: error?.name ?? "Error", message: String(error?.message ?? error), stack: error?.stack },
  }
}

export function compareArtifacts(left, right) {
  return {
    sameScenario: left.scenario === right.scenario,
    sameSdkVersion: left.sdkVersion === right.sdkVersion,
    metricDiff: diffMetrics(left.metrics, right.metrics),
    evidenceEqual: JSON.stringify(left.evidence) === JSON.stringify(right.evidence),
  }
}

export async function saveJson(path, value) {
  await mkdir(dirname(path), { recursive: true })
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`)
}

export async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"))
}

export function defaultPath(root, kind, scenario, variant) {
  return resolve(root, kind, `${scenario}--${variant}.json`)
}
