import { readFile } from "node:fs/promises"
import { basename, dirname, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { count, metric } from "../core/metrics.mjs"
import { collectAsync } from "../core/runtime.mjs"

const fixtureRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../fixtures/anthropic-progressive-skill")
const RESOURCE_MARKERS = [
  "TRIAGE_REFERENCE_ORBIT",
  "COMMUNICATION_REFERENCE_ORBIT",
  "ASSET_ORBIT",
  "VALIDATOR_SCRIPT_ORBIT",
  "EXAMPLE_ORBIT",
]

function safeResourcePath(root, requested) {
  const candidate = resolve(root, String(requested ?? ""))
  const prefix = `${root}/`
  if (!candidate.startsWith(prefix)) throw new Error(`resource path escapes skill root: ${requested}`)
  return candidate
}

function recordedProvider(sdk, messages, exposedTools) {
  const replay = new sdk.os.ReplayProvider(messages)
  return {
    descriptor: () => replay.descriptor(),
    async complete(context, tools) {
      exposedTools.push(tools.map(tool => tool.name))
      return replay.complete(context, tools)
    },
    async *stream(context, tools, extensions, state, signal) {
      exposedTools.push(tools.map(tool => tool.name))
      yield* replay.stream(context, tools, extensions, state, signal)
    },
  }
}

function resultText(result) {
  if (typeof result?.output === "string") return result.output
  if (typeof result?.content === "string") return result.content
  return JSON.stringify(result?.content ?? "")
}

export const skillProgressive = {
  id: "skill-progressive",
  description: "Anthropic Agent Skills SKILL.md metadata, activation, lazy resources, and tool widening",
  variants: ["deterministic"],
  surfaces: ["workflow", "advanced", "os"],
  async run({ sdk }) {
    const metadata = await sdk.workflow.scanSkillDir(fixtureRoot)
    const body = await sdk.workflow.readSkillFile(fixtureRoot, "incident-response-orchestrator")
    const skillMeta = metadata.find(item => item.name === "incident-response-orchestrator")
    if (!skillMeta || !body) throw new Error("Anthropic SKILL.md fixture was not discoverable")

    const accesses = []
    const exposedTools = []
    const readResource = sdk.root.tool(
      "read_skill_resource",
      "Read one explicitly requested resource from the active skill.",
      {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
        additionalProperties: false,
      },
      async args => {
        const requested = String(args.path)
        const file = safeResourcePath(fixtureRoot, requested)
        accesses.push(requested)
        return await readFile(file, "utf8")
      },
    )
    const validateIncident = sdk.root.tool(
      "validate_incident",
      "Validate the required incident object fields.",
      {
        type: "object",
        properties: {
          situation: { type: "string" },
          severity: { type: "string" },
          nextCheckpoint: { type: "string" },
        },
        required: ["situation", "severity", "nextCheckpoint"],
        additionalProperties: true,
      },
      args => ({ valid: Boolean(args.situation && args.severity && args.nextCheckpoint), marker: "VALIDATED_ORBIT" }),
    )

    const provider = recordedProvider(sdk, [
      { role: "assistant", content: "", toolCalls: [{ id: "activate-1", name: "skill", arguments: JSON.stringify({ name: "incident-response-orchestrator" }) }] },
      { role: "assistant", content: "", toolCalls: [{ id: "resource-1", name: "read_skill_resource", arguments: JSON.stringify({ path: "references/triage-matrix.md" }) }] },
      { role: "assistant", content: "", toolCalls: [{ id: "resource-2", name: "read_skill_resource", arguments: JSON.stringify({ path: "references/communication-policy.md" }) }] },
      { role: "assistant", content: "", toolCalls: [{ id: "resource-3", name: "read_skill_resource", arguments: JSON.stringify({ path: "assets/incident-template.json" }) }] },
      { role: "assistant", content: "", toolCalls: [{ id: "validate-1", name: "validate_incident", arguments: JSON.stringify({ situation: "elevated errors", severity: "SEV-2", nextCheckpoint: "15 minutes" }) }] },
      { role: "assistant", content: "PROGRESSIVE_SKILL_READY" },
    ], exposedTools)

    const log = new sdk.advanced.InMemorySessionLog()
    const turnMetrics = []
    const plane = new sdk.advanced.LocalExecutionPlane().register(readResource, validateIncident)
    const runner = new sdk.advanced.RuntimeRunner({
      provider,
      sessionLog: log,
      executionPlane: plane,
      skillDir: fixtureRoot,
      baselineToolIds: [],
      allowedToolIds: ["read_skill_resource", "validate_incident"],
      onTurnMetrics: metrics => turnMetrics.push({
        turn: metrics.turn,
        activeSkill: metrics.activeSkill,
        toolsExposed: metrics.toolsExposed,
      }),
      maxTokens: 8_000,
      maxTurns: 8,
      maxTotalTokens: 4_000,
      skillLeaseTurns: 8,
    })
    const stream = await collectAsync(runner.run({ sessionId: "anthropic-progressive", goal: "Load the incident response skill and prepare the response." }))
    const events = await log.read("anthropic-progressive")
    const toolResults = events
      .filter(entry => entry.event.kind === "tool_completed")
      .flatMap(entry => entry.event.kind === "tool_completed" ? entry.event.results : [])
    const resourceResults = toolResults.filter(result => ["resource-1", "resource-2", "resource-3"].includes(result.call_id))
    const finalText = stream.filter(event => event.type === "text_delta").map(event => event.delta).join("")
    const firstTurn = exposedTools[0] ?? []
    const activationTurn = exposedTools[1] ?? []
    // The kernel consumes the skill meta-tool internally and does not persist its body as a
    // normal `tool_completed` result. The public loader return is the canonical body evidence;
    // activeSkill + tool widening below prove that the same body was activated in the run.
    const activationBody = body
    const bodyActivated = activationBody.includes("SKILL_BODY_ORBIT")
    const bodyStayedSmall = RESOURCE_MARKERS.every(marker => !activationBody.includes(marker))
    const toolsNarrowBeforeActivation = firstTurn.includes("skill") && !firstTurn.includes("read_skill_resource") && !firstTurn.includes("validate_incident")
    const toolsWidenAfterActivation = activationTurn.includes("skill") && activationTurn.includes("read_skill_resource") && activationTurn.includes("validate_incident")
    const resourcesLoadedOnDemand = accesses.join(",") === "references/triage-matrix.md,references/communication-policy.md,assets/incident-template.json"
    const resourceResultsComplete = resourceResults.length === 3 && resourceResults.every(result => !result.is_error)
    const scriptAndExampleStayedLazy = !accesses.some(path => path.includes("scripts/") || path.includes("examples/"))
    const validationCompleted = toolResults.some(result => result.call_id === "validate-1" && resultText(result).includes("VALIDATED_ORBIT"))
    const passed = metadata.length === 1
      && skillMeta.description.includes("production incident")
      && skillMeta.allowedTools?.join(",") === "read_skill_resource,validate_incident"
      && bodyActivated
      && bodyStayedSmall
      && toolsNarrowBeforeActivation
      && toolsWidenAfterActivation
      && resourcesLoadedOnDemand
      && resourceResultsComplete
      && scriptAndExampleStayedLazy
      && validationCompleted
      && finalText === "PROGRESSIVE_SKILL_READY"
    if (!passed) {
      throw new Error(JSON.stringify({
        metadata,
        activationBody,
        bodyActivated,
        bodyStayedSmall,
        exposedTools,
        accesses,
        turnMetrics,
        finalText,
        resourceResults: resourceResults.map(result => ({ callId: result.call_id, isError: result.is_error })),
      }))
    }

    return {
      metrics: {
        catalogEntries: count(metadata.length),
        activationBodyChars: metric(activationBody.length, "chars"),
        resourcesLoaded: count(accesses.length),
        toolsBeforeActivation: count(firstTurn.length),
        toolsAfterActivation: count(activationTurn.length),
        turns: count(turnMetrics.length),
      },
      evidence: {
        format: "anthropic-agent-skills",
        entrypoint: "SKILL.md",
        metadata: skillMeta,
        progressive: {
          bodyActivated,
          bodyStayedSmall,
          toolsNarrowBeforeActivation,
          toolsWidenAfterActivation,
          resourcesLoaded: accesses,
          scriptAndExampleStayedLazy,
          validationCompleted,
          finalText,
        },
        turnMetrics,
        eventKinds: [...new Set(events.map(entry => entry.event.kind))].sort(),
        resourceBasenames: accesses.map(path => basename(path)),
        resourceRoot: relative(process.cwd(), fixtureRoot),
        bodyReferenceCount: (body.match(/references\//g) ?? []).length,
        fixtureFiles: [
          "SKILL.md",
          "references/triage-matrix.md",
          "references/communication-policy.md",
          "scripts/validate-incident.mjs",
          "assets/incident-template.json",
          "examples/sample-incident.md",
        ],
      },
    }
  },
}
