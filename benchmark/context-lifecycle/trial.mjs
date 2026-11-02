// Shared helpers for the context-lifecycle benchmarks (plan task 4.3): load a provider from `.env`,
// meter what each call sends, record every session event, and turn one task run into a trial line
// for `deepstrike-lab` (`cargo run -p deepstrike-lab -- report <file>`).
//
// A trial: { scenario, variant, task, success, error?, cost, token_estimates, lifecycle }.
// Keys are never printed: provider errors are redacted before they are recorded.

import { appendFile, readFile } from "node:fs/promises"
import { resolve } from "node:path"
import { pathToFileURL } from "node:url"

export const here = new URL("../..", import.meta.url).pathname

export function parseArgs(argv = process.argv.slice(2)) {
  return (name, fallback) => (argv.includes(name) ? argv[argv.indexOf(name) + 1] : fallback)
}

export async function loadEnv() {
  for (const line of (await readFile(resolve(here, ".env"), "utf8")).split(/\r?\n/)) {
    const match = line.trim().match(/^(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/)
    if (match && !(match[1] in process.env)) process.env[match[1]] = match[2].trim().replace(/^("|')(.*)\1$/, "$2")
  }
}

/** Import a module from a checkout's built Node SDK. */
export function sdk(repo) {
  return file => import(pathToFileURL(resolve(repo, "node/dist", file)).href)
}

export const redact = text => String(text)
  .replace(/Bearer\s+\S+/gi, "Bearer [REDACTED]")
  .replace(/sk-[A-Za-z0-9_-]{8,}/g, "sk-[REDACTED]")
  .slice(0, 240)

/** The real provider named by `--provider` (deepseek or openai), configured from `.env`. */
export async function realProvider(dist, name) {
  const retry = { maxRetries: 2, baseDelay: 1000 }
  if (name === "deepseek") {
    const model = process.env.DEEPSEEK_MODEL ?? "deepseek-chat"
    return {
      model,
      provider: (await dist("providers/public.js")).deepseek({
        apiKey: process.env.DEEPSEEK_API_KEY,
        model,
        ...(process.env.DEEPSEEK_BASE_URL ? { baseURL: process.env.DEEPSEEK_BASE_URL } : {}),
        retry,
      }),
    }
  }
  const root = await dist("index.js")
  const model = process.env.OPENAI_MODEL ?? "gpt-4o-mini"
  return {
    model,
    provider: new root.OpenAIProvider({
      apiKey: process.env.OPENAI_API_KEY,
      model,
      ...(process.env.OPENAI_BASE_URL ? { baseURL: process.env.OPENAI_BASE_URL } : {}),
      retry,
    }),
  }
}

/** Wraps a provider to count the characters each call sends and keep the last error. */
export class MeteredProvider {
  constructor(inner) {
    this.inner = inner
    this.reset()
  }
  reset() {
    this.sentChars = 0
    this.calls = 0
    this.lastError = undefined
    this.kernelEstimates = []
  }
  measure(context) {
    this.calls += 1
    // The kernel's own estimate of the request (0.2.80+); older checkouts do not send one.
    if (typeof context.estimatedTokens === "number") this.kernelEstimates.push(context.estimatedTokens)
    this.sentChars += JSON.stringify([
      context.systemStable ?? context.systemText ?? "",
      context.systemKnowledge ?? "",
      context.turns ?? [],
      context.stateTurn ?? "",
    ]).length
  }
  async complete(context, ...rest) {
    this.measure(context)
    try { return await this.inner.complete(context, ...rest) } catch (cause) { this.lastError = redact(cause?.message ?? cause); throw cause }
  }
  async *stream(context, ...rest) {
    this.measure(context)
    this.lastContext = context
    try { yield* this.inner.stream(context, ...rest) } catch (cause) { this.lastError = redact(cause?.message ?? cause); throw cause }
  }
}

/** A session log that also keeps every event it was given, across sessions (a workflow's nodes
 *  write to their own sessions). */
export function recordingSessionLog(inner) {
  const events = []
  return {
    events,
    log: new Proxy(inner, {
      get(target, property) {
        if (property === "append") {
          return async (sessionId, event) => {
            events.push(event)
            return target.append(sessionId, event)
          }
        }
        const value = target[property]
        return typeof value === "function" ? value.bind(target) : value
      },
    }),
  }
}

/** One trial line from a task run's recorded session events. */
export function trialFrom({ scenario, variant, task, success, error, provider, events }) {
  const attempts = events.filter(event => event.kind === "provider_attempt")
  const usage = attempts.map(attempt => attempt.usage).filter(Boolean)
  const sum = key => usage.reduce((total, entry) => total + (Number(entry[key]) || 0), 0)
  const observedByEffect = new Map(attempts
    .filter(attempt => attempt.usage?.inputTokens)
    .map(attempt => [attempt.effect_id, attempt.usage.inputTokens]))
  // Prefer the kernel's estimate, paired with the provider's counts in call order; a checkout that
  // does not send one falls back to the SDK's pre-call measurement, paired by effect id.
  const counted = attempts.filter(attempt => attempt.usage?.inputTokens).map(attempt => attempt.usage.inputTokens)
  const tokenEstimates = provider.kernelEstimates.length === counted.length && counted.length > 0
    ? counted.map((observed, index) => ({ estimated: provider.kernelEstimates[index], observed }))
    : events
      .filter(event => event.kind === "prompt_measured" && observedByEffect.has(event.effect_id))
      .map(event => ({ estimated: event.measurement.inputTokens, observed: observedByEffect.get(event.effect_id) }))
  return {
    scenario,
    variant,
    task,
    success,
    ...(error ? { error } : {}),
    cost: {
      sent_chars: provider.sentChars,
      calls: provider.calls,
      input_tokens: sum("inputTokens"),
      output_tokens: sum("outputTokens"),
      cache_read_tokens: sum("cacheReadInputTokens"),
    },
    token_estimates: tokenEstimates,
    lifecycle: events.filter(event => event.kind === "context_lifecycle_reported"),
  }
}

/** Append trials to `--out` as JSON Lines, or print them when no file is given. */
export async function writeTrials(out, trials) {
  const text = trials.map(trial => JSON.stringify(trial)).join("\n") + "\n"
  if (out) await appendFile(resolve(out), text)
  else process.stdout.write(text)
}
