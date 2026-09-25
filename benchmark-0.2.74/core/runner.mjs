import { resolve } from "node:path"
import { loadSdk, assertSdkVersion } from "./sdk.mjs"
import { makeArtifact, makeFailureArtifact, compareArtifacts, defaultPath, readJson, saveJson } from "./artifacts.mjs"
import { SCENARIO_MAP } from "../scenarios/index.mjs"

export async function runScenario(id, options = {}) {
  const scenario = SCENARIO_MAP.get(id)
  if (!scenario) throw new Error(`unknown scenario: ${id}`)
  const sdk = assertSdkVersion(options.sdk ?? await loadSdk())
  const variant = options.variant ?? scenario.variants[0]
  if (!scenario.variants.includes(variant)) throw new Error(`scenario ${id} does not support variant ${variant}`)
  const started = performance.now()
  try {
    const result = await scenario.run({ sdk, variant, options })
    return makeArtifact({ sdk, scenario: id, variant, result, elapsedMs: Math.round(performance.now() - started) })
  } catch (error) {
    return makeFailureArtifact({ sdk, scenario: id, variant, error, elapsedMs: Math.round(performance.now() - started) })
  }
}

export async function runScenarioOrThrow(id, options = {}) {
  const artifact = await runScenario(id, options)
  if (artifact.status !== "passed") throw new Error(`${id}/${artifact.variant}: ${artifact.error.message}`)
  return artifact
}

export async function compareScenario(id, options = {}) {
  const scenario = SCENARIO_MAP.get(id)
  if (!scenario) throw new Error(`unknown scenario: ${id}`)
  const variants = options.variants ?? scenario.variants
  const artifacts = []
  for (const variant of variants) artifacts.push(await runScenario(id, { ...options, variant }))
  return {
    scenario: id,
    sdkVersion: artifacts[0]?.sdkVersion,
    artifacts,
    comparisons: artifacts.slice(1).map((artifact, index) => compareArtifacts(artifacts[index], artifact)),
  }
}

export async function saveBaseline(root, artifact) {
  const path = defaultPath(root, "baselines", artifact.scenario, artifact.variant)
  await saveJson(path, artifact)
  return path
}

export async function checkBaseline(root, artifact) {
  const path = defaultPath(root, "baselines", artifact.scenario, artifact.variant)
  const baseline = await readJson(path)
  const comparison = compareArtifacts(baseline, artifact)
  return { path, baseline, artifact, comparison, passed: comparison.sameScenario && comparison.sameSdkVersion && comparison.evidenceEqual && comparison.metricDiff.every(diff => !diff.changed) }
}
