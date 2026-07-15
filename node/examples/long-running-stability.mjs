#!/usr/bin/env node

import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs"
import { mkdir, readFile, writeFile } from "node:fs/promises"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const exampleDir = path.dirname(fileURLToPath(import.meta.url))
const nodeRoot = path.resolve(exampleDir, "..")
const repoRoot = path.resolve(nodeRoot, "..")

loadEnvFile(path.join(exampleDir, ".env"))
loadEnvFile(path.join(nodeRoot, ".env"))
loadEnvFile(path.join(repoRoot, ".env"))

const dryRun = process.argv.includes("--dry-run")
const config = readConfig()
const runRoot = path.resolve(exampleDir, ".stability-runs", config.sessionId)
const stepsPath = path.join(runRoot, "steps.jsonl")
const artifactsDir = path.join(runRoot, "artifacts")
mkdirSync(artifactsDir, { recursive: true })

const sdk = await loadSdk()
const {
  FileArchiveStore,
  FileSessionLog,
  LocalExecutionPlane,
  OllamaProvider,
  RuntimeRunner,
  createProvider,
  rebuildOsSnapshotFromSessionEvents,
  tool,
} = sdk
const { LargeResultSpool } = await loadSdkModule("runtime/large-result-spool.js")

const sessionLog = new FileSessionLog(path.join(runRoot, "sessions"))
const dreamStore = createJsonDreamStore(path.join(runRoot, "memory"))
const knowledgeSource = createStaticKnowledgeSource()
const executionPlane = new LocalExecutionPlane().register(
  buildRecordStepTool(config, stepsPath),
  buildEmitLargePayloadTool(config, artifactsDir),
  buildVerifyCheckpointTool(config, stepsPath),
)

await seedMemory(dreamStore, config.agentId, config.sessionId)

const runtimeOptions = {
  provider: dryRun ? createDryRunProvider() : createLlmProvider(config, { createProvider, OllamaProvider }),
  sessionLog,
  executionPlane,
  maxTokens: config.maxTokens,
  maxTurns: config.maxTurns,
  timeoutMs: config.timeoutMs,
  agentId: config.agentId,
  systemPrompt: systemPrompt(config),
  initialMemory: [
    "Stability validation objective: preserve ordered progress under many tool turns.",
    `Run directory: ${runRoot}`,
  ],
  skillDir: path.join(exampleDir, "skills"),
  dreamStore,
  knowledgeSource,
  schedulerPolicy: {
    version: 1,
    criticalPathWeight: 1_000_000,
    fanoutWeight: 10_000,
    ageWeight: 1_000,
    tokenCostWeight: 1,
  },
  resourceQuota: {
    maxConcurrentSubagents: config.maxConcurrentSubagents,
    maxSpawnDepth: config.maxSpawnDepth,
    memoryWritesPerWindow: {
      maxWrites: config.memoryWritesPerWindow,
      windowMs: config.memoryWriteWindowMs,
    },
  },
  resultSpool: new LargeResultSpool({
    spoolDir: path.join(runRoot, "spool"),
    spoolThresholdBytes: config.spoolThresholdBytes,
    previewTokens: config.spoolPreviewTokens,
  }),
  compressionStore: new FileArchiveStore(path.join(runRoot, "archives")),
  enablePlanTool: true,
  milestonePolicy: "auto_pass",
  onPermissionRequest: async request => ({
    approved: true,
    responder: "stability-demo",
    reason: `auto-approved for ${request.toolName}`,
  }),
  enableDiagnosticsDashboard: config.diagnostics,
}

printConfig(config, runRoot, dryRun)

if (dryRun) {
  const latestSeq = await sessionLog.latestSeq(config.sessionId)
  const memories = await dreamStore.loadMemories(config.agentId)
  const knowledge = await knowledgeSource.retrieve("stability memory skill", 3)
  console.log("\nDry run completed.")
  console.log(JSON.stringify({
    sessionId: config.sessionId,
    latestSeq,
    memoryCount: memories.length,
    knowledgeSnippets: knowledge.length,
    tools: executionPlane.schemas().map(schema => schema.name),
    skillDir: runtimeOptions.skillDir,
  }, null, 2))
  process.exit(0)
}

const runner = new RuntimeRunner(runtimeOptions)
const stream = config.wake
  ? runner.wake(config.sessionId)
  : runner.run({
      sessionId: config.sessionId,
      goal: buildGoal(config),
      criteria: [
        `record_step has been called for steps 1 through ${config.targetSteps}`,
        "checkpoint validation reports no missing steps",
        "the final response summarizes stability status and observed recovery signals",
      ],
    })

let doneEvent = null
let toolCallCount = 0
let toolResultCount = 0
let textBytes = 0

for await (const event of stream) {
  if (event.type === "text_delta") {
    const delta = String(event.delta ?? "")
    textBytes += Buffer.byteLength(delta)
    process.stdout.write(delta)
  } else if (event.type === "tool_call") {
    toolCallCount += 1
    console.log(`\n[tool_call] ${event.name} ${JSON.stringify(event.arguments ?? {})}`)
  } else if (event.type === "tool_result") {
    toolResultCount += 1
    const content = String(event.content ?? "")
    console.log(`[tool_result] ${event.name ?? event.callId} ${content.slice(0, 220).replace(/\s+/g, " ")}`)
  } else if (event.type === "tool_argument_repaired") {
    console.log(`[tool_argument_repaired] ${event.name}`)
  } else if (event.type === "permission_request") {
    console.log(`[permission_request] ${event.toolName}: ${event.reason}`)
  } else if (event.type === "permission_resolved") {
    console.log(`[permission_resolved] ${event.toolName}: ${event.approved}`)
  } else if (event.type === "error") {
    console.error(`\n[error] ${redactSecret(String(event.message ?? ""))}`)
  } else if (event.type === "done") {
    doneEvent = event
  }
}

await printRunSummary({
  sessionLog,
  sessionId: config.sessionId,
  stepsPath,
  runRoot,
  rebuildOsSnapshotFromSessionEvents,
  doneEvent,
  streamCounts: { toolCallCount, toolResultCount, textBytes },
})

function readConfig() {
  const provider = env("DEEPSTRIKE_PROVIDER", "openai")
  const defaultModel = defaultModelForProvider(provider)
  return {
    provider,
    endpoint: env("DEEPSTRIKE_ENDPOINT", ""),
    model: env("DEEPSTRIKE_MODEL", defaultModel),
    baseURL: env("DEEPSTRIKE_BASE_URL", ""),
    apiKey: env("DEEPSTRIKE_API_KEY", ""),
    sessionId: env("DEEPSTRIKE_SESSION_ID", `node-stability-${timestampId()}`),
    agentId: env("DEEPSTRIKE_AGENT_ID", "node-stability-agent"),
    wake: boolEnv("DEEPSTRIKE_WAKE", false),
    targetSteps: intEnv("DEEPSTRIKE_TARGET_STEPS", 30, 1, 500),
    checkpointEvery: intEnv("DEEPSTRIKE_CHECKPOINT_EVERY", 5, 1, 100),
    largeResultEvery: intEnv("DEEPSTRIKE_LARGE_RESULT_EVERY", 10, 0, 500),
    largeResultKib: intEnv("DEEPSTRIKE_LARGE_RESULT_KIB", 80, 1, 1024),
    maxTurns: intEnv("DEEPSTRIKE_MAX_TURNS", 90, 1, 1000),
    maxTokens: intEnv("DEEPSTRIKE_MAX_TOKENS", 160000, 1000, 2000000),
    timeoutMs: intEnv("DEEPSTRIKE_TIMEOUT_MS", 30 * 60 * 1000, 1000, 24 * 60 * 60 * 1000),
    maxConcurrentSubagents: intEnv("DEEPSTRIKE_MAX_CONCURRENT_SUBAGENTS", 2, 0, 100),
    maxSpawnDepth: intEnv("DEEPSTRIKE_MAX_SPAWN_DEPTH", 2, 0, 20),
    memoryWritesPerWindow: intEnv("DEEPSTRIKE_MEMORY_WRITES_PER_WINDOW", 200, 0, 100000),
    memoryWriteWindowMs: intEnv("DEEPSTRIKE_MEMORY_WRITE_WINDOW_MS", 60000, 1, 24 * 60 * 60 * 1000),
    spoolThresholdBytes: intEnv("DEEPSTRIKE_SPOOL_THRESHOLD_BYTES", 50 * 1024, 1024, 10 * 1024 * 1024),
    spoolPreviewTokens: intEnv("DEEPSTRIKE_SPOOL_PREVIEW_TOKENS", 500, 1, 10000),
    diagnostics: boolEnv("DEEPSTRIKE_DIAGNOSTICS", false),
  }
}

function defaultModelForProvider(provider) {
  const defaults = {
    anthropic: "anthropic/claude-3-5-haiku-latest",
    openai: "openai/gpt-4o-mini",
    minimax: "minimax/MiniMax-M2",
    deepseek: "deepseek/deepseek-chat",
    kimi: "kimi/moonshot-v1-32k",
    qwen: "qwen/qwen3.5-flash",
    gemini: "gemini/gemini-2.0-flash",
    glm: "glm/glm-4-flash",
    ollama: "llama3",
  }
  return defaults[provider] ?? "openai/gpt-4o-mini"
}

async function loadSdk() {
  const sdkPath = path.join(nodeRoot, "dist", "index.js")
  if (!existsSync(sdkPath)) {
    throw new Error(`Node SDK dist not found at ${sdkPath}. Run: npm run build --prefix node`)
  }
  return import(pathToFileURL(sdkPath).href)
}

async function loadSdkModule(relativePath) {
  const modulePath = path.join(nodeRoot, "dist", relativePath)
  if (!existsSync(modulePath)) {
    throw new Error(`Node SDK dist module not found at ${modulePath}. Run: npm run build --prefix node`)
  }
  return import(pathToFileURL(modulePath).href)
}

function createLlmProvider(config, sdkProviders) {
  if (config.provider === "ollama") {
    return new sdkProviders.OllamaProvider(config.model, config.baseURL || "http://localhost:11434")
  }
  const apiKey = config.apiKey || providerApiKey(config.provider)
  if (!apiKey) {
    throw new Error(`Missing API key. Set DEEPSTRIKE_API_KEY or ${providerEnvKey(config.provider)} in node/examples/.env`)
  }
  return sdkProviders.createProvider({
    provider: config.provider,
    model: config.model,
    apiKey,
    ...(config.endpoint ? { endpoint: config.endpoint } : {}),
    ...(config.baseURL ? { baseURL: config.baseURL } : {}),
    retry: {
      maxRetries: intEnv("DEEPSTRIKE_PROVIDER_RETRIES", 2, 0, 10),
      baseDelay: intEnv("DEEPSTRIKE_PROVIDER_RETRY_DELAY_MS", 500, 1, 60000),
    },
  })
}

function providerApiKey(provider) {
  const keys = provider === "qwen"
    ? ["DEEPSTRIKE_API_KEY", "QWEN_API_KEY", "DASHSCOPE_API_KEY"]
    : provider === "gemini"
      ? ["DEEPSTRIKE_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"]
      : ["DEEPSTRIKE_API_KEY", providerEnvKey(provider)]
  for (const key of keys) {
    if (process.env[key]) return process.env[key]
  }
  return ""
}

function providerEnvKey(provider) {
  return `${provider.toUpperCase()}_API_KEY`
}

function systemPrompt(config) {
  return [
    "You are executing a DeepStrike Node SDK stability validation run.",
    "Use tools deliberately and keep the run ordered. Call at most one validation tool per assistant turn unless the goal explicitly asks for a checkpoint or large payload at that step.",
    "When the skill meta-tool is available, read the stability-drill skill before the first record_step call.",
    "Use memory and knowledge retrieval once early in the run, then continue the ordered step loop.",
    `Target steps: ${config.targetSteps}. Checkpoint every ${config.checkpointEvery} step(s).`,
  ].join("\n")
}

function buildGoal(config) {
  const largeRule = config.largeResultEvery > 0
    ? `After record_step on every ${config.largeResultEvery}th step, call emit_large_payload for that same step.`
    : "Do not call emit_large_payload in this run."
  return [
    `Run a long-running stability drill for exactly ${config.targetSteps} ordered steps.`,
    "Before step 1, call the skill meta-tool for stability-drill, call knowledge with query \"DeepStrike stability validation\", and call memory with query \"previous stability run\".",
    "For each step N from 1 to the target, call record_step once with step=N and a short observation about continuity, context, memory, or tool stability.",
    `After every ${config.checkpointEvery}th step, call verify_checkpoint.`,
    largeRule,
    "Do not skip ahead. If a checkpoint reports missing steps, fill the missing steps before continuing.",
    "After the final checkpoint, answer with a concise stability summary, including any skipped, missing, spooled, compressed, or budget events you observed.",
  ].join("\n")
}

function buildRecordStepTool(config, filePath) {
  return tool(
    "record_step",
    "Record one ordered stability drill step. Call this once per step in ascending order.",
    {
      type: "object",
      properties: {
        step: { type: "integer" },
        observation: { type: "string" },
      },
      required: ["step", "observation"],
    },
    async args => {
      const step = asInteger(args.step)
      if (step < 1 || step > config.targetSteps) {
        return JSON.stringify({ ok: false, error: `step ${step} outside 1..${config.targetSteps}` })
      }
      const prior = readSteps(filePath)
      const duplicate = prior.some(entry => entry.step === step)
      const missingBefore = []
      for (let i = 1; i < step; i++) {
        if (!prior.some(entry => entry.step === i)) missingBefore.push(i)
      }
      const record = {
        step,
        duplicate,
        missingBefore,
        observation: String(args.observation).slice(0, 1000),
        recordedAt: new Date().toISOString(),
      }
      appendFileSync(filePath, `${JSON.stringify(record)}\n`, "utf8")
      return JSON.stringify({
        ok: missingBefore.length === 0 && !duplicate,
        recorded: step,
        duplicate,
        missingBefore,
        nextStep: Math.min(config.targetSteps, step + 1),
        remaining: Math.max(0, config.targetSteps - step),
      })
    },
  )
}

function buildEmitLargePayloadTool(config, dir) {
  return tool(
    "emit_large_payload",
    "Emit and persist a large deterministic payload to exercise large-result spooling.",
    {
      type: "object",
      properties: {
        step: { type: "integer" },
        label: { type: "string", default: "stability-payload" },
        kib: { type: "integer", default: config.largeResultKib },
      },
      required: ["step"],
    },
    async args => {
      const step = asInteger(args.step)
      const kib = clamp(asInteger(args.kib ?? config.largeResultKib), 1, 1024)
      const label = String(args.label ?? "stability-payload").replace(/[^a-zA-Z0-9._-]/g, "_")
      const targetBytes = kib * 1024
      const header = `payload label=${label} step=${step} target_kib=${kib}\n`
      const line = `step=${step} label=${label} marker=deepstrike-node-stability context-retention-check\n`
      let payload = header
      while (Buffer.byteLength(payload) < targetBytes) payload += line
      const filePath = path.join(dir, `payload-step-${step}.txt`)
      writeFileSync(filePath, payload, "utf8")
      return payload
    },
  )
}

function buildVerifyCheckpointTool(config, filePath) {
  return tool(
    "verify_checkpoint",
    "Verify recorded step continuity and report missing or duplicate steps.",
    {
      type: "object",
      properties: {
        step: { type: "integer" },
      },
      required: ["step"],
    },
    async args => {
      const step = clamp(asInteger(args.step), 1, config.targetSteps)
      const records = readSteps(filePath)
      const seen = new Map()
      for (const record of records) {
        seen.set(record.step, (seen.get(record.step) ?? 0) + 1)
      }
      const missing = []
      const duplicates = []
      for (let i = 1; i <= step; i++) {
        const count = seen.get(i) ?? 0
        if (count === 0) missing.push(i)
        if (count > 1) duplicates.push(i)
      }
      return JSON.stringify({
        ok: missing.length === 0 && duplicates.length === 0,
        checkedThrough: step,
        recordedTotal: records.length,
        uniqueRecorded: seen.size,
        missing,
        duplicates,
        finalExpected: config.targetSteps,
      })
    },
  )
}

async function printRunSummary(params) {
  const entries = await params.sessionLog.read(params.sessionId)
  const events = entries.map(entry => entry.event)
  const eventCounts = countBy(events.map(event => event.kind))
  const steps = readSteps(params.stepsPath)
  const snapshot = params.rebuildOsSnapshotFromSessionEvents(events)
  const latestTerminal = [...events].reverse().find(event => event.kind === "run_terminal")
  console.log("\n\nRun summary")
  console.log(JSON.stringify({
    sessionId: params.sessionId,
    runRoot: params.runRoot,
    doneEvent: params.doneEvent,
    latestTerminal,
    streamCounts: params.streamCounts,
    sessionEvents: events.length,
    eventCounts,
    recordedSteps: steps.length,
    lastRecordedStep: steps.at(-1)?.step ?? 0,
    osSnapshot: snapshot,
  }, null, 2))
}

async function seedMemory(store, agentId, sessionId) {
  const existing = await store.loadMemories(agentId)
  if (existing.length > 0) return
  const now = Date.now()
  await store.commit(agentId, {
    toAdd: [{
      record_id: "stability_seed",
      scope: { tenant_id: "examples", namespace: agentId },
      name: "stability_seed",
      kind: "project",
      content: "Previous stability runs should verify ordered tool calls, checkpoint continuity, large-result spooling, and wake replay.",
      description: "Stability-run verification checklist",
      provenance: {
        author: "host",
        trust: "host_verified",
        evidence_refs: [],
        session_id: sessionId,
      },
      created_at: now,
      updated_at: now,
      recall_count: 0,
      confidence: 1,
      links: [],
      pinned: true,
    }],
    toRemoveIndices: [],
    stats: {
      insightsProcessed: 1,
      duplicatesRemoved: 0,
      conflictsResolved: 0,
      entriesAdded: 1,
    },
  }, existing)
}

function createJsonDreamStore(root) {
  const storePath = (agentId, fileName) => path.join(root, sanitizeFileName(agentId), fileName)
  return {
    async loadSessions(agentId) {
      return readJson(storePath(agentId, "sessions.json"), [])
    },
    async loadMemories(agentId) {
      return readJson(storePath(agentId, "memories.json"), [])
    },
    async commit(agentId, result, existing) {
      const removals = new Set(result.toRemoveIndices ?? [])
      const retained = existing.filter((_, index) => !removals.has(index))
      const next = [...retained]
      for (const incoming of result.toAdd ?? []) {
        const index = next.findIndex(record =>
          record.scope.tenant_id === incoming.scope.tenant_id
          && record.scope.namespace === incoming.scope.namespace
          && record.kind === incoming.kind
          && record.name === incoming.name)
        if (index >= 0) next[index] = incoming
        else next.push(incoming)
      }
      await writeJson(storePath(agentId, "memories.json"), next)
    },
    async search(agentId, query) {
      const memories = await this.loadMemories(agentId)
      const terms = tokenize(query.query)
      return memories
        .filter(memory => memory.scope.tenant_id === query.scope.tenant_id
          && memory.scope.namespace === query.scope.namespace
          && (query.kinds.length === 0 || query.kinds.includes(memory.kind)))
        .map(memory => ({ memory, score: scoreText(`${memory.name} ${memory.description} ${memory.content}`, terms) }))
        .filter(entry => entry.score > 0)
        .sort((a, b) => b.score - a.score)
        .slice(0, query.top_k)
        .map(entry => ({ record: entry.memory, score: entry.score, why: "lexical match" }))
    },
    async saveSession(data) {
      const sessions = await this.loadSessions(data.agentId)
      sessions.push(data)
      await writeJson(storePath(data.agentId, "sessions.json"), sessions)
    },
  }
}

function createStaticKnowledgeSource() {
  const docs = [
    "DeepStrike stability validation should exercise LLM turns, tool calls, session log replay, skill loading, memory retrieval, knowledge retrieval, compression, and large-result spooling.",
    "A healthy long run records monotonic progress, preserves checkpoint evidence, and terminates with run_terminal rather than provider timeout or malformed replay.",
    "Large tool outputs above the spool threshold should be represented in context as previews while the full content is persisted under the configured spool directory.",
    "Wake replay should rebuild context from the JSONL session log and continue a mid-run session without duplicating earlier completed tool results.",
  ]
  return {
    async retrieve(query, topK = 5) {
      const terms = tokenize(query)
      return docs
        .map(doc => ({ doc, score: scoreText(doc, terms) }))
        .sort((a, b) => b.score - a.score)
        .slice(0, topK)
        .map(entry => entry.doc)
    },
  }
}

function createDryRunProvider() {
  return {
    runtimePolicy() {
      return { maxTurns: 1 }
    },
    async *stream() {
      yield { type: "text_delta", delta: "dry-run provider is not used for real validation" }
    },
  }
}

function loadEnvFile(filePath) {
  if (!existsSync(filePath)) return
  const content = readFileSync(filePath, "utf8")
  for (const rawLine of content.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line || line.startsWith("#")) continue
    const normalized = line.startsWith("export ") ? line.slice("export ".length).trim() : line
    const eq = normalized.indexOf("=")
    if (eq <= 0) continue
    const key = normalized.slice(0, eq).trim()
    let value = normalized.slice(eq + 1).trim()
    if ((value.startsWith("\"") && value.endsWith("\"")) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1)
    }
    if (!(key in process.env)) process.env[key] = value
  }
}

function env(name, fallback) {
  const value = process.env[name]
  return value === undefined || value === "" ? fallback : value
}

function boolEnv(name, fallback) {
  const value = process.env[name]
  if (value === undefined || value === "") return fallback
  return ["1", "true", "yes", "on"].includes(value.toLowerCase())
}

function intEnv(name, fallback, min, max) {
  const parsed = Number.parseInt(env(name, String(fallback)), 10)
  if (!Number.isFinite(parsed)) return fallback
  return clamp(parsed, min, max)
}

function asInteger(value) {
  const parsed = Number.parseInt(String(value), 10)
  return Number.isFinite(parsed) ? parsed : 0
}

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value))
}

function readSteps(filePath) {
  if (!existsSync(filePath)) return []
  return readFileSync(filePath, "utf8")
    .split(/\r?\n/)
    .filter(Boolean)
    .map(line => JSON.parse(line))
}

async function readJson(filePath, fallback) {
  try {
    return JSON.parse(await readFile(filePath, "utf8"))
  } catch (error) {
    if (error?.code === "ENOENT") return fallback
    throw error
  }
}

async function writeJson(filePath, data) {
  await mkdir(path.dirname(filePath), { recursive: true })
  await writeFile(filePath, `${JSON.stringify(data, null, 2)}\n`, "utf8")
}

function tokenize(text) {
  return String(text)
    .toLowerCase()
    .split(/[^a-z0-9_]+/)
    .filter(token => token.length > 2)
}

function scoreText(text, terms) {
  const lower = String(text).toLowerCase()
  if (terms.length === 0) return 1
  return terms.reduce((score, term) => score + (lower.includes(term) ? 1 : 0), 0)
}

function countBy(values) {
  const counts = {}
  for (const value of values) counts[value] = (counts[value] ?? 0) + 1
  return counts
}

function sanitizeFileName(value) {
  return String(value).replace(/[^a-zA-Z0-9._-]/g, "_")
}

function timestampId() {
  return new Date().toISOString().replace(/[-:.TZ]/g, "").slice(0, 14)
}

function printConfig(config, runRoot, isDryRun) {
  console.log("DeepStrike Node stability demo")
  console.log(JSON.stringify({
    dryRun: isDryRun,
    provider: config.provider,
    endpoint: config.endpoint || null,
    model: config.model,
    sessionId: config.sessionId,
    wake: config.wake,
    runRoot,
    targetSteps: config.targetSteps,
    checkpointEvery: config.checkpointEvery,
    largeResultEvery: config.largeResultEvery,
    largeResultKib: config.largeResultKib,
    maxTurns: config.maxTurns,
    timeoutMs: config.timeoutMs,
  }, null, 2))
}

function redactSecret(text) {
  return text
    .replace(/sk-[a-zA-Z0-9_*.-]{8,}/g, "sk-[redacted]")
    .replace(/(api[_-]?key provided:\s*)[^.\s]+/gi, "$1[redacted]")
}
