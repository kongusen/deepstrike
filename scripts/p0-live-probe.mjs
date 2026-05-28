#!/usr/bin/env node
/** Quick probe: one chat completion via .env credentials */
import { readFileSync } from "node:fs"
import { OpenAIChatProvider } from "../node/dist/index.js"

loadEnvFile(new URL("../.env", import.meta.url))

const apiKey = process.env.OPENAI_API_KEY
const baseURL = process.env.OPENAI_BASE_URL
const model = process.env.OPENAI_MODEL ?? "gpt-4o-mini"

if (!apiKey) {
  console.log(JSON.stringify({ pass: false, error: "OPENAI_API_KEY missing" }))
  process.exit(1)
}

const provider = new OpenAIChatProvider(apiKey, model, { maxRetries: 1, baseDelay: 500 }, baseURL)
const t0 = performance.now()

try {
  const msg = await Promise.race([
    provider.complete(
      { systemText: "Be brief.", systemStable: "Be brief.", systemVolatile: "", turns: [{ role: "user", content: "Reply exactly: PONG" }] },
      [],
    ),
    new Promise((_, rej) => setTimeout(() => rej(new Error("timeout 60s")), 60_000)),
  ])
  console.log(JSON.stringify({
    pass: true,
    model,
    baseURL: baseURL ?? "(default)",
    elapsed_ms: Math.round(performance.now() - t0),
    content: msg.content?.slice(0, 100),
    tokenCount: msg.tokenCount,
    note: "xiaoai.plus reachable; no extra HTTP proxy needed if this passes",
  }, null, 2))
} catch (err) {
  console.log(JSON.stringify({
    pass: false,
    model,
    baseURL: baseURL ?? "(default)",
    elapsed_ms: Math.round(performance.now() - t0),
    error: String(err),
  }, null, 2))
  process.exit(1)
}

function loadEnvFile(url) {
  try {
    for (const line of readFileSync(url, "utf8").split(/\r?\n/)) {
      const t = line.trim()
      if (!t || t.startsWith("#")) continue
      const i = t.indexOf("=")
      if (i < 0) continue
      const k = t.slice(0, i), v = t.slice(i + 1)
      if (!(k in process.env)) process.env[k] = v
    }
  } catch { /* optional */ }
}
