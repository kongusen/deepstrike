#!/usr/bin/env node
import { resolve } from "node:path"
import { SCENARIOS } from "../scenarios/index.mjs"
import { compareScenario, checkBaseline, runScenario, saveBaseline } from "../core/runner.mjs"
import { loadProjectEnv } from "../core/env.mjs"

const root = resolve(new URL("..", import.meta.url).pathname)
const [, , command = "list", ...args] = process.argv
const flags = new Set(args.filter(arg => arg.startsWith("--")))
const valueFlag = name => args.find(arg => arg.startsWith(`${name}=`))?.slice(name.length + 1)
const numericFlag = name => {
  const value = valueFlag(name)
  if (value === undefined) return undefined
  const parsed = Number(value)
  if (!Number.isFinite(parsed) || parsed < 0) throw new Error(`${name} must be a non-negative number`)
  return parsed
}

if (command === "list") {
  for (const scenario of SCENARIOS) console.log(`${scenario.id}\t${scenario.variants.join(", ")}\t${scenario.description}`)
} else {
  const scenarioId = command
  const scenario = SCENARIOS.find(item => item.id === scenarioId)
  if (!scenario) {
    console.error(`unknown scenario: ${scenarioId}`)
    process.exitCode = 2
  } else {
    const variant = valueFlag("--variant") ?? scenario.variants[0]
    if (scenario.requiresLive && !flags.has("--live")) {
      console.error(`${scenarioId} requires --live; this command may make external provider requests`)
      process.exitCode = 2
    } else {
      if (scenario.requiresLive) await loadProjectEnv(root)
      const provider = valueFlag("--provider")
      const timeoutMs = numericFlag("--timeout-ms")
      const maxTotalTokens = numericFlag("--max-total-tokens")
      const result = flags.has("--compare")
        ? await compareScenario(scenarioId, { provider, timeoutMs, maxTotalTokens })
        : await runScenario(scenarioId, { variant, provider, timeoutMs, maxTotalTokens })
      if (flags.has("--baseline-save")) {
        const artifacts = result.artifacts ?? [result]
        for (const artifact of artifacts) console.log(`baseline: ${await saveBaseline(root, artifact)}`)
      }
      if (flags.has("--baseline-check")) {
        const artifacts = result.artifacts ?? [result]
        for (const artifact of artifacts) {
          const check = await checkBaseline(root, artifact)
          if (!check.passed) {
            console.error(JSON.stringify(check, null, 2))
            process.exitCode = 1
          }
        }
      }
      console.log(JSON.stringify(result, null, 2))
      if ((result.status && result.status !== "passed") || result.artifacts?.some(artifact => artifact.status !== "passed")) process.exitCode = 1
    }
  }
}
