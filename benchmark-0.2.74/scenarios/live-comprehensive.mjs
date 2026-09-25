import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { tmpdir } from "node:os"
import { count, metric, ratio } from "../core/metrics.mjs"
import { collectAsync } from "../core/runtime.mjs"
import { providerFromEnv, redactError, withTimeout } from "./live-smoke.mjs"

const scope = { tenant_id: "benchmark-live", namespace: "comprehensive" }

function passed(name, evidence = {}) {
  return { name, status: "passed", ...evidence }
}

function notExercised(name, reason, evidence = {}) {
  return { name, status: "not_exercised", reason, ...evidence }
}

async function runFeature(name, fn) {
  try {
    return await fn()
  } catch (error) {
    return { name, status: "failed", error: redactError(error) }
  }
}

function makeAgent(sdk, name, provider, sessionLog, options = {}) {
  return sdk.root.createAgent({
    name,
    instructions: options.instructions ?? "Follow the requested output exactly and keep responses short.",
    ...(options.tools ? { tools: options.tools } : {}),
    ...(options.skills ? { skills: options.skills } : {}),
    ...(options.knowledge ? { knowledge: options.knowledge } : {}),
    ...(options.outputSchema ? { outputSchema: options.outputSchema } : {}),
    ...(options.memoryStore ? { memoryStore: options.memoryStore, memoryScope: scope } : {}),
    runtimeBinding: {
      provider,
      executionPlane: options.executionPlane ?? new sdk.advanced.LocalExecutionPlane(),
      sessionLog,
      ...(options.runtimeOptions ? { runtimeOptions: options.runtimeOptions } : {}),
    },
  })
}

async function runCore(sdk, options) {
  const built = providerFromEnv(sdk, "openai")
  if (!built.provider) return notExercised("core", built.skipReason)
  const log = new sdk.advanced.InMemorySessionLog()
  const agent = makeAgent(sdk, "live-comprehensive-core", built.provider, log)
  const basic = await withTimeout(signal => agent.run("Reply exactly CORE_OK.", { session: { id: "comprehensive-core" }, maxTurns: 2, maxTotalTokens: options.maxTotalTokens, signal }), options.timeoutMs)
  const stream = await withTimeout(signal => collectAsync(agent.stream("Reply exactly STREAM_OK.", { session: { id: "comprehensive-stream" }, maxTurns: 2, maxTotalTokens: options.maxTotalTokens, signal })), options.timeoutMs)
  const session = agent.session("comprehensive-session")
  const sessionRun = await withTimeout(signal => session.run("Reply exactly SESSION_OK.", { maxTurns: 2, maxTotalTokens: options.maxTotalTokens, signal }), options.timeoutMs)
  const streamPassed = stream.some(event => event.type === "text_delta") && stream.some(event => event.type === "done")
  const basicPassed = basic.status === "completed" && /CORE_OK/i.test(basic.output)
  const sessionPassed = sessionRun.status === "completed" && sessionRun.sessionId === "comprehensive-session"
  if (!basicPassed || !streamPassed || !sessionPassed) return { name: "core", status: "failed", basicStatus: basic.status, streamTypes: stream.map(event => event.type), sessionStatus: sessionRun.status }
  return passed("core", {
    basicStatus: basic.status,
    streamTypes: [...new Set(stream.map(event => event.type))],
    sessionId: sessionRun.sessionId,
    evidenceFields: Object.keys(basic.evidence ?? {}).length,
    measurementAvailable: Boolean(basic.evidence?.measurement),
  })
}

async function runTool(sdk, options) {
  const built = providerFromEnv(sdk, "openai")
  if (!built.provider) return notExercised("tool", built.skipReason)
  const log = new sdk.advanced.InMemorySessionLog()
  const add = sdk.root.tool("add_numbers", "Add two numbers", {
    type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"], additionalProperties: false,
  }, args => `TOOL_SUM=${Number(args.a) + Number(args.b)}`)
  const agent = makeAgent(sdk, "live-comprehensive-tool", built.provider, log, {
    tools: [add],
    executionPlane: new sdk.advanced.LocalExecutionPlane().register(add),
  })
  const result = await withTimeout(signal => agent.run("Call add_numbers exactly once with a=2 and b=3, then reply exactly TOOL_OK.", {
    session: { id: "comprehensive-tool" }, maxTurns: 4, maxTotalTokens: options.maxTotalTokens, signal,
  }), options.timeoutMs)
  const events = await log.read("comprehensive-tool")
  const requested = events.filter(entry => entry.event.kind === "tool_requested")
  const completed = events.filter(entry => entry.event.kind === "tool_completed")
  const callCount = requested.reduce((sum, entry) => entry.event.kind === "tool_requested" ? sum + entry.event.calls.length : sum, 0)
  if (callCount === 0) return notExercised("tool", "model did not request the declared tool", { runStatus: result.status, outputPrefix: String(result.output).slice(0, 80) })
  const errors = completed.flatMap(entry => entry.event.kind === "tool_completed" ? entry.event.results.filter(item => item.is_error) : [])
  if (errors.length > 0 || result.status !== "completed") return { name: "tool", status: "failed", callCount, runStatus: result.status }
  return passed("tool", { callCount, completed: completed.length, outputPrefix: String(result.output).slice(0, 80) })
}

async function runMemory(sdk, options) {
  const built = providerFromEnv(sdk, "openai")
  if (!built.provider) return notExercised("memory", built.skipReason)
  const store = new sdk.memory.InMemoryMemoryStore()
  const log = new sdk.advanced.InMemorySessionLog()
  const agent = makeAgent(sdk, "live-comprehensive-memory", built.provider, log, { memoryStore: store })
  const saved = await agent.remember({ name: "release-codename", content: "The release codename is ORBIT.", kind: "project" })
  const recalled = await agent.recall("release codename")
  const result = await withTimeout(signal => agent.run("You must call the memory tool before answering. Do not infer or answer from your own knowledge. Query for 'release codename', read the returned memory, then reply exactly MEMORY_ORBIT.", {
    session: { id: "comprehensive-memory" }, maxTurns: 4, maxTotalTokens: options.maxTotalTokens, signal,
  }), options.timeoutMs)
  const events = await log.read("comprehensive-memory")
  const calls = events.filter(entry => entry.event.kind === "tool_requested").flatMap(entry => entry.event.kind === "tool_requested" ? entry.event.calls : [])
  if (recalled[0]?.record.record_id !== saved.record_id) return { name: "memory", status: "failed", reason: "host remember/recall mismatch" }
  const terminal = events.find(entry => entry.event.kind === "run_terminal")
  const attempt = events.find(entry => entry.event.kind === "provider_attempt")
  const memoryToolCalls = calls.filter(call => call.name === "memory").length
  const memoryRetrieved = events.some(entry => entry.event.kind === "memory_retrieval_result")
  if (memoryToolCalls > 0 && memoryRetrieved) return passed("memory", {
    hostRecallHits: recalled.length,
    memoryToolCalls,
    memoryRetrieved,
    modelRunStatus: result.status,
    completionWithinBudget: result.status === "completed",
    ...(terminal?.event.kind === "run_terminal" ? { terminalReason: terminal.event.reason } : {}),
  })
  if (result.status !== "completed") return {
    name: "memory",
    status: "failed",
    hostRecallHits: recalled.length,
    runStatus: result.status,
    eventKinds: [...new Set(events.map(entry => entry.event.kind))].sort(),
    ...(terminal?.event.kind === "run_terminal" ? { terminalReason: terminal.event.reason } : {}),
    ...(attempt?.event.kind === "provider_attempt" ? { providerErrorClass: attempt.event.last_error_class } : {}),
  }
  return notExercised("memory", "model did not request the memory tool", { hostRecallHits: recalled.length, runStatus: result.status })
}

async function runKnowledge(sdk, options) {
  const built = providerFromEnv(sdk, "openai")
  if (!built.provider) return notExercised("knowledge", built.skipReason)
  const log = new sdk.advanced.InMemorySessionLog()
  const agent = makeAgent(sdk, "live-comprehensive-knowledge", built.provider, log, {
    knowledge: [{ id: "benchmark-fact", name: "Benchmark Fact", source: { kind: "text", content: "The benchmark verification phrase is KNOWLEDGE_ORBIT." } }],
  })
  const result = await withTimeout(signal => agent.run("Use the knowledge tool to retrieve the benchmark verification phrase, then reply exactly KNOWLEDGE_ORBIT.", {
    session: { id: "comprehensive-knowledge" }, maxTurns: 4, maxTotalTokens: options.maxTotalTokens, signal,
  }), options.timeoutMs)
  const events = await log.read("comprehensive-knowledge")
  const calls = events.filter(entry => entry.event.kind === "tool_requested").flatMap(entry => entry.event.kind === "tool_requested" ? entry.event.calls : [])
  if (result.status !== "completed") return { name: "knowledge", status: "failed", runStatus: result.status, eventKinds: [...new Set(events.map(entry => entry.event.kind))].sort() }
  if (!calls.some(call => call.name === "knowledge")) return notExercised("knowledge", "model did not request the knowledge tool", { runStatus: result.status })
  return passed("knowledge", { knowledgeToolCalls: calls.filter(call => call.name === "knowledge").length, outputPrefix: String(result.output).slice(0, 80) })
}

async function runSkill(sdk, options) {
  const built = providerFromEnv(sdk, "openai")
  if (!built.provider) return notExercised("skill", built.skipReason)
  const skillDir = await mkdtemp(join(tmpdir(), "deepstrike-live-skill-"))
  await writeFile(join(skillDir, "benchmark-skill.md"), "---\nname: benchmark-skill\ndescription: A benchmark verification skill\n---\nThe skill verification phrase is SKILL_ORBIT.")
  const skillMetadata = await sdk.workflow.scanSkillDir(skillDir)
  const skillBody = await sdk.workflow.readSkillFile(skillDir, "benchmark-skill")
  const loaderPassed = skillMetadata.some(item => item.name === "benchmark-skill") && skillBody?.includes("SKILL_ORBIT")
  const log = new sdk.advanced.InMemorySessionLog()
  try {
    const agent = makeAgent(sdk, "live-comprehensive-skill", built.provider, log, {
      skills: [{ name: "benchmark-skill", description: "A benchmark verification skill", instructions: "The skill verification phrase is SKILL_ORBIT. Return it when asked." }],
    })
    const declarationPassed = agent.declaration.skills?.some(skill => skill.name === "benchmark-skill") === true
    const result = await withTimeout(signal => agent.run("You must call the skill tool before answering. Use the skill name benchmark-skill. Do not answer until the skill tool has returned, then reply exactly SKILL_ORBIT.", {
      session: { id: "comprehensive-skill" }, maxTurns: 5, maxTotalTokens: options.maxTotalTokens, signal,
    }), options.timeoutMs)
    const events = await log.read("comprehensive-skill")
    const calls = events.filter(entry => entry.event.kind === "tool_requested").flatMap(entry => entry.event.kind === "tool_requested" ? entry.event.calls : [])
    const skillToolCalls = calls.filter(call => call.name === "skill").length
    const skillCompleted = events.some(entry => entry.event.kind === "tool_completed")
    if (!loaderPassed || !declarationPassed) return { name: "skill", status: "failed", reason: "skill loader or declaration failed" }
    if (skillToolCalls > 0 && skillCompleted) return passed("skill", { loaderPassed, declarationPassed, skillToolCalls, modelRunStatus: result.status, completionWithinBudget: result.status === "completed", outputPrefix: String(result.output).slice(0, 80) })
    return passed("skill", { loaderPassed, declarationPassed, modelToolStatus: "not_exercised", modelRunStatus: result.status })
  } finally {
    await rm(skillDir, { recursive: true, force: true })
  }
}

async function runOutputSchema(sdk, options) {
  const built = providerFromEnv(sdk, "openai")
  if (!built.provider) return notExercised("output-schema", built.skipReason)
  const agent = makeAgent(sdk, "live-comprehensive-schema", built.provider, new sdk.advanced.InMemorySessionLog(), {
    outputSchema: { type: "object", properties: { answer: { type: "string" } }, required: ["answer"], additionalProperties: false },
  })
  const result = await withTimeout(signal => agent.run("Return a JSON object with answer exactly SCHEMA_ORBIT.", { session: { id: "comprehensive-schema" }, maxTurns: 3, maxTotalTokens: options.maxTotalTokens, signal }), options.timeoutMs)
  if (result.status !== "completed" || !result.outputValidation?.ok) return { name: "output-schema", status: "failed", runStatus: result.status, validation: result.outputValidation }
  return passed("output-schema", { validation: result.outputValidation, outputType: typeof result.output })
}

async function runDynamicWorkflow(sdk, options) {
  const built = providerFromEnv(sdk, "openai")
  if (!built.provider) return notExercised("dynamic-workflow", built.skipReason)
  const log = new sdk.advanced.InMemorySessionLog()
  const runner = new sdk.advanced.RuntimeRunner({
    provider: built.provider,
    sessionLog: log,
    executionPlane: new sdk.advanced.LocalExecutionPlane(),
    maxTokens: options.maxTotalTokens ?? 1_200,
    maxTurns: 4,
    subAgentOrchestrator: sdk.workflow.defaultSubAgentOrchestrator,
  })
  const run = await withTimeout(signal => runner.runDynamicWorkflow(ctx => ctx.parallel(["a", "b"], item => ctx.agent(`Return exactly DYNAMIC_${item.toUpperCase()}.`, { label: item })), {
    runId: "comprehensive-dynamic",
    sessionId: "comprehensive-dynamic",
    limits: { maxAgentsPerRun: 2, maxConcurrentAgents: 2 },
    signal,
  }), options.timeoutMs)
  const values = run.value.map(item => item?.text ?? "")
  if (run.progress.status !== "completed" || run.progress.agentsCompleted !== 2 || !values.every(value => /^DYNAMIC_[AB]/.test(value))) {
    return { name: "dynamic-workflow", status: "failed", progress: run.progress, values }
  }
  return passed("dynamic-workflow", { progress: run.progress, lifecycle: run.events.map(event => event.kind), values })
}

export const liveComprehensive = {
  id: "live-comprehensive",
  description: "OpenAI live capability matrix for Agent, tools, memory, knowledge, skills, schema, and dynamic workflow",
  variants: ["openai"],
  surfaces: ["root", "advanced", "workflow", "memory", "harness", "os"],
  requiresLive: true,
  async run({ sdk, options }) {
    const liveOptions = { ...options, timeoutMs: options.timeoutMs ?? 90_000, maxTotalTokens: options.maxTotalTokens ?? 1_200 }
    const allFeatures = [
      ["core", runCore],
      ["tool", runTool],
      ["memory", runMemory],
      ["knowledge", runKnowledge],
      ["skill", runSkill],
      ["output-schema", runOutputSchema],
      ["dynamic-workflow", runDynamicWorkflow],
    ]
    const requestedFeatures = options.features
      ? new Set(String(options.features).split(",").map(value => value.trim()).filter(Boolean))
      : null
    const selectedFeatures = requestedFeatures ? allFeatures.filter(([name]) => requestedFeatures.has(name)) : allFeatures
    const features = []
    for (const [featureName, feature] of selectedFeatures) {
      features.push(await runFeature(featureName, () => feature(sdk, liveOptions)))
    }
    const exercised = features.filter(feature => feature.status !== "not_exercised")
    const passedFeatures = features.filter(feature => feature.status === "passed")
    const failedFeatures = features.filter(feature => feature.status === "failed")
    return {
      status: failedFeatures.length === 0 && exercised.length > 0 ? "passed" : "failed",
      metrics: {
        features: count(features.length),
        featuresPassed: count(passedFeatures.length),
        featuresFailed: count(failedFeatures.length),
        featuresNotExercised: count(features.filter(feature => feature.status === "not_exercised").length),
        featurePassRate: ratio(passedFeatures.length, exercised.length),
        dynamicAgentsCompleted: metric(features.find(feature => feature.name === "dynamic-workflow")?.progress?.agentsCompleted ?? 0, "agents"),
      },
      evidence: { provider: "openai", model: process.env.OPENAI_MODEL ?? "gpt-4o-mini", features },
      ...(failedFeatures.length > 0 ? { error: `${failedFeatures.length} live feature(s) failed` } : {}),
    }
  },
}
