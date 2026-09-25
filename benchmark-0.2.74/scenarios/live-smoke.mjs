import { count, metric, ratio } from "../core/metrics.mjs"
import { collectAsync } from "../core/runtime.mjs"

const PROVIDER_SPECS = {
  openai: { key: "OPENAI_API_KEY", model: "OPENAI_MODEL", baseURL: "OPENAI_BASE_URL" },
  deepseek: { key: "DEEPSEEK_API_KEY", model: "DEEPSEEK_MODEL", baseURL: "DEEPSEEK_BASE_URL" },
  minimax: { key: "MINIMAX_API_KEY", model: "MINIMAX_MODEL", baseURL: "MINIMAX_BASE_URL" },
  glm: { key: "GLM_API_KEY", model: "GLM_MODEL", baseURL: "GLM_BASE_URL" },
  kimi: { key: "KIMI_API_KEY", model: "KIMI_MODEL", baseURL: "KIMI_BASE_URL" },
}

export function redactError(error) {
  return String(error?.message ?? error)
    .replace(/Bearer\s+[^\s]+/gi, "Bearer [REDACTED]")
    .replace(/(?:sk|api|key)[-_][A-Za-z0-9._-]{8,}/gi, "[REDACTED]")
}

export function providerFromEnv(sdk, name) {
  const spec = PROVIDER_SPECS[name]
  const apiKey = process.env[spec.key]
  if (!apiKey) return { provider: null, skipReason: `missing ${spec.key}` }
  const model = process.env[spec.model]
  const baseURL = process.env[spec.baseURL]
  const retry = { maxRetries: 1, baseDelay: 500 }
  if (name === "openai") {
    return {
      provider: new sdk.root.OpenAIProvider({
        apiKey,
        model: model ?? "gpt-4o-mini",
        ...(baseURL ? { baseURL } : {}),
        retry,
      }),
      model: model ?? "gpt-4o-mini",
    }
  }
  const factory = sdk.providers[name]
  if (typeof factory !== "function") throw new Error(`public providers barrel has no ${name} factory`)
  return {
    provider: factory({
      apiKey,
      ...(model ? { model } : {}),
      ...(baseURL ? { baseURL } : {}),
      retry,
    }),
    model,
  }
}

export async function withTimeout(task, timeoutMs) {
  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(new Error(`live smoke timeout after ${timeoutMs}ms`)), timeoutMs)
  try {
    return await task(controller.signal)
  } finally {
    clearTimeout(timeout)
  }
}

async function probeProvider(sdk, name, options) {
  const started = performance.now()
  const built = providerFromEnv(sdk, name)
  if (!built.provider) return { name, status: "skipped", reason: built.skipReason }

  const descriptor = typeof built.provider.descriptor === "function" ? built.provider.descriptor() : {}
  const sessionLog = new sdk.advanced.InMemorySessionLog()
  const add = sdk.root.tool("add_numbers", "Add two numbers and return TOOL_SUM=5", {
    type: "object",
    properties: { a: { type: "number" }, b: { type: "number" } },
    required: ["a", "b"],
    additionalProperties: false,
  }, args => `TOOL_SUM=${Number(args.a) + Number(args.b)}`)
  const plane = new sdk.advanced.LocalExecutionPlane().register(add)
  const agent = sdk.root.createAgent({
    name: `live-${name}`,
    model: built.model,
    instructions: "Follow the requested output exactly and keep responses short.",
    tools: [add],
    runtimeBinding: { provider: built.provider, executionPlane: plane, sessionLog },
  })

  const result = {
    name,
    status: "failed",
    provider: { provider: descriptor.provider, protocol: descriptor.protocol, model: descriptor.model },
  }
  try {
    const basic = await withTimeout(signal => agent.run("Reply with exactly PONG and no punctuation.", {
      session: { id: `live-${name}-basic` }, maxTurns: 2, maxTotalTokens: options.maxTotalTokens, signal,
    }), options.timeoutMs)
    const basicEvents = await sessionLog.read(`live-${name}-basic`)
    result.basic = {
      passed: basic.status === "completed" && /\bPONG\b/i.test(basic.output),
      status: basic.status,
      outputPrefix: String(basic.output).slice(0, 80),
      usageAvailable: Boolean(basic.usage),
      routeAvailable: Boolean(basic.evidence?.route),
      measurementAvailable: Boolean(basic.evidence?.measurement),
      evidenceFields: Object.keys(basic.evidence ?? {}).length,
      eventKinds: [...new Set(basicEvents.map(entry => entry.event.kind))].sort(),
      ...(basicEvents.find(entry => entry.event.kind === "run_terminal")?.event.kind === "run_terminal"
        ? { terminalReason: basicEvents.find(entry => entry.event.kind === "run_terminal").event.reason }
        : {}),
      ...(basicEvents.find(entry => entry.event.kind === "provider_attempt")?.event.kind === "provider_attempt"
        ? { providerAttemptStatus: basicEvents.find(entry => entry.event.kind === "provider_attempt").event.status, providerErrorClass: basicEvents.find(entry => entry.event.kind === "provider_attempt").event.last_error_class }
        : {}),
    }

    const streamEvents = await withTimeout(signal => collectAsync(agent.stream("Reply with exactly STREAM_OK.", {
      session: { id: `live-${name}-stream` }, maxTurns: 2, maxTotalTokens: options.maxTotalTokens, signal,
    })), options.timeoutMs)
    result.stream = {
      passed: streamEvents.some(event => event.type === "text_delta") && streamEvents.some(event => event.type === "done"),
      eventTypes: [...new Set(streamEvents.map(event => event.type))],
      textChars: streamEvents.filter(event => event.type === "text_delta").reduce((n, event) => n + String(event.delta ?? "").length, 0),
      ...(streamEvents.find(event => event.type === "error")?.message ? { error: redactError(streamEvents.find(event => event.type === "error").message) } : {}),
    }

    const toolRun = await withTimeout(signal => agent.run("Call add_numbers exactly once with a=2 and b=3, then reply with exactly TOOL_OK.", {
      session: { id: `live-${name}-tool` }, maxTurns: 4, maxTotalTokens: options.maxTotalTokens, signal,
    }), options.timeoutMs)
    const toolEvents = await sessionLog.read(`live-${name}-tool`)
    const toolRequested = toolEvents.filter(entry => entry.event.kind === "tool_requested")
    const toolCompleted = toolEvents.filter(entry => entry.event.kind === "tool_completed")
    const toolErrors = toolCompleted.flatMap(entry => entry.event.kind === "tool_completed"
      ? entry.event.results.filter(item => item.is_error)
      : [])
    result.tool = {
      status: toolRequested.length > 0 ? (toolErrors.length === 0 ? "exercised" : "failed") : "not_exercised",
      requested: toolRequested.reduce((n, entry) => entry.event.kind === "tool_requested" ? n + entry.event.calls.length : n, 0),
      completed: toolCompleted.reduce((n, entry) => entry.event.kind === "tool_completed" ? n + entry.event.results.length : n, 0),
      outputPrefix: String(toolRun.output).slice(0, 80),
      runStatus: toolRun.status,
    }
    result.status = result.basic.passed && result.stream.passed && result.basic.evidenceFields >= 2 ? "passed" : "failed"
  } catch (error) {
    result.status = "failed"
    result.error = redactError(error)
  }
  result.elapsedMs = Math.round(performance.now() - started)
  return result
}

export const liveSmoke = {
  id: "live-smoke",
  description: "real provider authentication, streaming, evidence, and tool capability",
  variants: ["configured"],
  surfaces: ["root", "providers", "advanced", "os"],
  requiresLive: true,
  async run({ sdk, options }) {
    const liveOptions = {
      ...options,
      timeoutMs: options.timeoutMs ?? 90_000,
      maxTotalTokens: options.maxTotalTokens ?? 1_200,
    }
    const requested = String(options.provider ?? process.env.LLM_PROVIDER ?? "all").toLowerCase()
    const names = requested === "all" ? Object.keys(PROVIDER_SPECS) : requested.split(",").map(value => value.trim()).filter(Boolean)
    const reports = []
    for (const name of names) {
      if (!PROVIDER_SPECS[name]) {
        reports.push({ name, status: "failed", error: `unknown provider ${name}` })
        continue
      }
      try {
        reports.push(await probeProvider(sdk, name, liveOptions))
      } catch (error) {
        reports.push({ name, status: "failed", error: redactError(error) })
      }
    }
    const attempted = reports.filter(report => report.status !== "skipped")
    const passed = attempted.filter(report => report.status === "passed")
    const failed = attempted.filter(report => report.status === "failed")
    const basicPassed = reports.filter(report => report.basic?.passed).length
    const streamPassed = reports.filter(report => report.stream?.passed).length
    const toolExercised = reports.filter(report => report.tool?.status === "exercised").length
    const providerUsageAvailable = reports.filter(report => report.basic?.usageAvailable).length
    const measurementAvailable = reports.filter(report => report.basic?.measurementAvailable).length
    const result = {
      status: failed.length === 0 && attempted.length > 0 ? "passed" : "failed",
      metrics: {
        providersAttempted: count(attempted.length),
        providersPassed: count(passed.length),
        providersFailed: count(failed.length),
        basicPassRate: ratio(basicPassed, attempted.length),
        streamPassRate: ratio(streamPassed, attempted.length),
        toolCapabilityRate: ratio(toolExercised, attempted.length),
        providerUsageAvailabilityRate: ratio(providerUsageAvailable, attempted.length),
        measurementAvailabilityRate: ratio(measurementAvailable, attempted.length),
        averageLatencyMs: metric(attempted.length ? Math.round(attempted.reduce((sum, report) => sum + (report.elapsedMs ?? 0), 0) / attempted.length) : 0, "ms"),
      },
      evidence: { requestedProviders: names, reports },
    }
    if (failed.length > 0) result.error = `${failed.length} provider(s) failed live smoke`
    return result
  },
}
