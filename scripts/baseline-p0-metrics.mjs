#!/usr/bin/env node
/**
 * P0-2 live baseline: Anthropic prompt cache hit verification.
 *
 * Usage: node scripts/baseline-p0-metrics.mjs
 * Requires: ANTHROPIC_API_KEY in .env or environment
 */
import { readFileSync } from "node:fs"

loadEnvFile(new URL("../.env", import.meta.url))

const apiKey = process.env.ANTHROPIC_API_KEY
const model = process.env.ANTHROPIC_MODEL ?? "claude-haiku-4-5"

if (!apiKey) {
  console.log(JSON.stringify({
    skipped: true,
    reason: "ANTHROPIC_API_KEY not set — add to .env to verify cache_read_input_tokens",
    hint: "Current .env uses OpenAI; P0-2 caching is Anthropic-only",
  }, null, 2))
  process.exit(0)
}

const systemStable = "You are a concise counting assistant. Always reply with one word."
const systemVolatile = "[TASK STATE] goal: count sequentially"

async function callAnthropic(messages) {
  const resp = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: {
      "x-api-key": apiKey,
      "anthropic-version": "2023-06-01",
      "anthropic-beta": "prompt-caching-2024-07-31",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      model,
      max_tokens: 32,
      system: [{ type: "text", text: systemStable, cache_control: { type: "ephemeral" } }],
      messages: messages.map(m => ({
        role: m.role,
        content: m.role === "user" && m.reminder
          ? `${m.content}\n\n[SYSTEM REMINDER]\n${systemVolatile}`
          : m.content,
      })),
    }),
  })
  if (!resp.ok) throw new Error(`Anthropic ${resp.status}: ${await resp.text()}`)
  return resp.json()
}

const rounds = []
let history = [{ role: "user", content: "Say 'one'.", reminder: true }]

for (let round = 1; round <= 3; round++) {
  const t0 = performance.now()
  const data = await callAnthropic(history)
  const text = data.content?.find(b => b.type === "text")?.text ?? ""
  rounds.push({
    round,
    latency_ms: Math.round(performance.now() - t0),
    input_tokens: data.usage.input_tokens,
    output_tokens: data.usage.output_tokens,
    cache_read_input_tokens: data.usage.cache_read_input_tokens ?? 0,
    cache_creation_input_tokens: data.usage.cache_creation_input_tokens ?? 0,
    text_preview: text.slice(0, 30),
  })
  history = [
    ...history,
    { role: "assistant", content: text },
    { role: "user", content: `Round ${round + 1}: next number.`, reminder: true },
  ]
}

const pass = rounds.slice(1).every(r => r.cache_read_input_tokens > 0)
console.log(JSON.stringify({ model, pass, rounds }, null, 2))

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
    // optional
  }
}
