import { readFile } from "node:fs/promises"
import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { metric } from "../core/metrics.mjs"
import { collectAsync } from "../core/runtime.mjs"
import { providerFromEnv, redactError } from "./live-smoke.mjs"

const fixtureRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../fixtures/anthropic-progressive-skill")
const observedCount = value => metric(value, "count", { mode: "observed" })

function safeResourcePath(root, requested) {
  const candidate = resolve(root, String(requested ?? ""))
  if (!candidate.startsWith(`${root}/`)) throw new Error(`resource path escapes skill root: ${requested}`)
  return candidate
}

export const liveSkillProgressive = {
  id: "live-skill-progressive",
  description: "real provider progressive activation of an Anthropic Agent Skills fixture",
  variants: ["configured"],
  surfaces: ["workflow", "advanced", "os"],
  requiresLive: true,
  async run({ sdk, options }) {
    const providerName = String(options.provider ?? "openai").toLowerCase()
    if (!["openai", "minimax", "kimi"].includes(providerName)) {
      return { status: "failed", error: `provider ${providerName} is not supported by this scenario; use openai, minimax, or kimi` }
    }
    const built = providerFromEnv(sdk, providerName)
    if (!built.provider) return { status: "failed", error: built.skipReason }
    const timeoutMs = options.timeoutMs ?? 90_000
    const maxTotalTokens = options.maxTotalTokens ?? 2_000
    const accesses = []
    const turnMetrics = []
    const readResource = sdk.root.tool(
      "read_skill_resource",
      "Read one explicitly requested resource from the active incident-response skill. Use only the paths named by the skill.",
      {
        type: "object", properties: { path: { type: "string" } }, required: ["path"], additionalProperties: false,
      },
      async args => {
        const requested = String(args.path)
        accesses.push(requested)
        return await readFile(safeResourcePath(fixtureRoot, requested), "utf8")
      },
    )
    const validateIncident = sdk.root.tool(
      "validate_incident",
      "Validate the required incident object fields before finalizing an incident response.",
      {
        type: "object",
        properties: { situation: { type: "string" }, severity: { type: "string" }, nextCheckpoint: { type: "string" } },
        required: ["situation", "severity", "nextCheckpoint"],
        additionalProperties: true,
      },
      args => ({ valid: Boolean(args.situation && args.severity && args.nextCheckpoint), marker: "VALIDATED_ORBIT" }),
    )
    const log = new sdk.advanced.InMemorySessionLog()
    const runner = new sdk.advanced.RuntimeRunner({
      provider: built.provider,
      sessionLog: log,
      executionPlane: new sdk.advanced.LocalExecutionPlane().register(readResource, validateIncident),
      skillDir: fixtureRoot,
      baselineToolIds: [],
      allowedToolIds: ["read_skill_resource", "validate_incident"],
      skillLeaseTurns: 8,
      maxTokens: 8_000,
      maxTurns: 8,
      maxTotalTokens,
      timeoutMs,
      onTurnMetrics: metrics => turnMetrics.push({ turn: metrics.turn, activeSkill: metrics.activeSkill, toolsExposed: metrics.toolsExposed }),
    })
    const prompt = [
      "Use the progressive skill catalog for this task.",
      "First call the skill tool with name incident-response-orchestrator.",
      "After activation, read references/triage-matrix.md and assets/incident-template.json with read_skill_resource, then call validate_incident with situation='elevated errors', severity='SEV-2', nextCheckpoint='15 minutes'.",
      "Do not read scripts or examples. After the tool results, reply exactly LIVE_PROGRESSIVE_OK.",
    ].join(" ")
    let stream
    let error
    try {
      stream = await collectAsync(runner.run({ sessionId: "live-anthropic-progressive", goal: prompt }))
    } catch (cause) {
      error = redactError(cause)
    }
    const events = await log.read("live-anthropic-progressive")
    const skillCalls = events
      .filter(entry => entry.event.kind === "tool_requested")
      .flatMap(entry => entry.event.kind === "tool_requested" ? entry.event.calls : [])
      .filter(call => call.name === "skill")
    const toolErrors = events
      .filter(entry => entry.event.kind === "tool_completed")
      .flatMap(entry => entry.event.kind === "tool_completed" ? entry.event.results.filter(result => result.is_error) : [])
    const finalText = (stream ?? []).filter(event => event.type === "text_delta").map(event => event.delta).join("")
    const runTerminal = events.find(entry => entry.event.kind === "run_terminal")?.event
    const activationSignals = turnMetrics.filter(metrics => metrics.activeSkill === "incident-response-orchestrator").length
    const activated = skillCalls.length > 0 || activationSignals > 0
    const resourcesLoaded = accesses.length > 0
    const completed = runTerminal?.reason === "completed" && finalText.includes("LIVE_PROGRESSIVE_OK")
    const status = activated && resourcesLoaded && toolErrors.length === 0 ? "passed" : "failed"
    return {
      status,
      metrics: {
        skillActivations: observedCount(activationSignals),
        persistedSkillToolCalls: observedCount(skillCalls.length),
        resourcesLoaded: observedCount(accesses.length),
        toolErrors: observedCount(toolErrors.length),
        completionWarnings: observedCount(activated && !completed ? 1 : 0),
        turns: observedCount(turnMetrics.length),
        outputChars: metric(finalText.length, "chars", { mode: "observed" }),
      },
      evidence: {
        provider: providerName,
        model: built.model,
        fixture: "benchmark-0.2.74/fixtures/anthropic-progressive-skill",
        activated,
        modelToolStatus: activated ? "exercised" : "not_exercised",
        resourcesLoaded: accesses,
        toolErrors: toolErrors.length,
        terminalReason: runTerminal?.reason,
        finalText: finalText.slice(0, 160),
        turnMetrics,
        eventKinds: [...new Set(events.map(entry => entry.event.kind))].sort(),
        ...(error ? { error } : {}),
      },
    }
  },
}
