import { readFileSync } from "node:fs"
import { Agent, DeepSeekProvider, MiniMaxProvider, tool } from "../node/dist/index.js"

loadEnvFile(new URL("../.env", import.meta.url))

const smokeCases = [
  {
    id: "minimax",
    apiKeyEnv: "MINIMAX_API_KEY",
    createProvider: apiKey => new MiniMaxProvider(apiKey),
    code: "BLUE-4721",
    extensions: undefined,
  },
  {
    id: "deepseek",
    apiKeyEnv: "DEEPSEEK_API_KEY",
    createProvider: apiKey => new DeepSeekProvider(apiKey, "deepseek-v4-flash"),
    code: "GREEN-8842",
    extensions: { exposeReasoning: true },
  },
]

const results = []
for (const smoke of smokeCases) {
  const apiKey = process.env[smoke.apiKeyEnv]
  if (!apiKey) {
    results.push({ provider: smoke.id, skipped: true, reason: `missing ${smoke.apiKeyEnv}` })
    continue
  }

  try {
    results.push(await runToolLoopSmoke(smoke, apiKey))
  } catch (error) {
    results.push({ provider: smoke.id, error: String(error) })
  }
}

console.log(JSON.stringify(results, null, 2))

async function runToolLoopSmoke(smoke, apiKey) {
  const agent = new Agent(smoke.createProvider(apiKey), { maxTokens: 4096, maxTurns: 4 })
  agent.register(tool("lookup_code", "Return the exact verification code.", {
    type: "object",
    properties: {},
    required: [],
  }, async () => smoke.code))

  const events = []
  for await (const evt of agent.runStreaming(
    "Call lookup_code exactly once, then reply with only the returned code.",
    undefined,
    smoke.extensions,
  )) {
    events.push(evt)
  }

  return {
    provider: smoke.id,
    toolCalls: events.filter(e => e.type === "tool_call").length,
    toolResults: events.filter(e => e.type === "tool_result").length,
    thinkingDeltas: events.filter(e => e.type === "thinking_delta").length,
    done: events.find(e => e.type === "done") ?? null,
    text: events.filter(e => e.type === "text_delta").map(e => e.delta).join(""),
    errors: events.filter(e => e.type === "error").map(e => e.message),
  }
}

function loadEnvFile(url) {
  try {
    const raw = readFileSync(url, "utf8")
    for (const line of raw.split(/\r?\n/)) {
      const trimmed = line.trim()
      if (!trimmed || trimmed.startsWith("#")) continue
      const index = trimmed.indexOf("=")
      if (index === -1) continue
      const key = trimmed.slice(0, index)
      const value = trimmed.slice(index + 1)
      if (!(key in process.env)) process.env[key] = value
    }
  } catch {
    // `.env` is optional; missing keys are reported as skipped cases below.
  }
}
