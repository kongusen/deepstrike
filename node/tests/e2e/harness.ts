/**
 * E2E test harness for deepstrike kernel mechanisms.
 *
 * MetricCapturingProvider wraps a real provider and records per-turn metrics
 * (token counts, cache stats, context snapshot). E2EHarness drives the runner
 * and returns a structured HarnessResult for scenario validation.
 */
import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import type { RegisteredTool } from "../../src/tools/index.js"
import type {
  LLMProvider,
  Message,
  RenderedContext,
  StreamEvent,
  ToolSchema,
} from "../../src/types.js"
import type { SessionEvent } from "../../src/runtime/session-log.js"

// ── per-turn metrics ─────────────────────────────────────────────────────────

export interface TurnMetrics {
  turn: number
  /** Prompt token count reported by the provider API (the authoritative rho numerator). */
  inputTokens: number
  outputTokens: number
  /** Compression that fired at the end of this turn (if any). */
  compressionAction?: string
  /** Snapshot of what the provider saw — used to validate context shape. */
  contextSnapshot?: {
    turnsCount: number
    systemKnowledge: string
    stateTurnContent: string
  }
  /** State turn snapshot from the next LLM call after compression on this turn. */
  postCompressionSnapshot?: {
    turnsCount: number
    systemKnowledge: string
    stateTurnContent: string
  }
}

// ── scenario definition ───────────────────────────────────────────────────────

export interface ScenarioCfg {
  id: string
  name: string
  goal: string
  criteria?: string[]
  tools?: RegisteredTool[]
  maxTokens: number
  maxTurns: number
  timeoutMs?: number
  systemPrompt?: string
  /** Injected into Slot 2 (system_knowledge) via initialMemory. */
  initialMemory?: string[]
  validate(result: HarnessResult): { passed: boolean; failure?: string }
}

// ── run result ────────────────────────────────────────────────────────────────

export interface HarnessResult {
  id: string
  passed: boolean
  failure?: string
  turnsUsed: number
  compressions: number
  compressionActions: string[]
  /** Highest inputTokens seen in any turn (proxy for peak rho). */
  peakInputTokens: number
  /** Text collected from all text_delta events. */
  finalText: string
  finalStatus: string
  metrics: TurnMetrics[]
  events: Array<{ seq: number; event: SessionEvent }>
}

// ── metric-capturing provider wrapper ────────────────────────────────────────

export class MetricCapturingProvider implements LLMProvider {
  readonly turnMetrics: TurnMetrics[] = []
  private turnIndex = 0

  constructor(private inner: LLMProvider) {}

  async complete(ctx: RenderedContext, tools: ToolSchema[]): Promise<Message> {
    return this.inner.complete(ctx, tools)
  }

  async *stream(ctx: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    const turn = this.turnIndex++
    let inputTokens = 0
    let outputTokens = 0

    const snapshot = {
      turnsCount: ctx.turns.length,
      systemKnowledge: ctx.systemKnowledge ?? "",
      stateTurnContent: ctx.turns[0]?.content ?? "",
    }

    try {
      for await (const event of this.inner.stream(ctx, tools)) {
        if (event.type === "usage") {
          const u = event as { type: string; inputTokens?: number; outputTokens?: number }
          inputTokens = u.inputTokens ?? inputTokens
          outputTokens = u.outputTokens ?? outputTokens
        }
        yield event
      }
    } finally {
      // Record even when the provider throws — snapshot reflects post-compression context.
      this.turnMetrics.push({ turn, inputTokens, outputTokens, contextSnapshot: snapshot })
    }
  }
}

// ── harness ───────────────────────────────────────────────────────────────────

export async function runScenario(
  provider: LLMProvider,
  cfg: ScenarioCfg,
): Promise<HarnessResult> {
  const capturing = new MetricCapturingProvider(provider)
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  for (const t of cfg.tools ?? []) plane.register(t)

  const runner = new RuntimeRunner({
    provider: capturing,
    sessionLog,
    executionPlane: plane,
    maxTokens: cfg.maxTokens,
    maxTurns: cfg.maxTurns,
    systemPrompt: cfg.systemPrompt,
    initialMemory: cfg.initialMemory,
  })

  const sid = `e2e-${cfg.id}-${Date.now()}`
  let finalText = ""
  let finalStatus = "error"

  const timeout = cfg.timeoutMs ?? 120_000
  const runPromise = (async () => {
    for await (const evt of runner.run({ sessionId: sid, goal: cfg.goal, criteria: cfg.criteria })) {
      if (evt.type === "text_delta") finalText += (evt as { delta: string }).delta
      if (evt.type === "done") finalStatus = (evt as { status: string }).status
    }
  })()

  await Promise.race([
    runPromise,
    new Promise<void>((_, reject) => setTimeout(() => reject(new Error("scenario timeout")), timeout)),
  ])

  const events = await sessionLog.read(sid)

  // Correlate compression events back to turn metrics.
  // Kernel turn increments after each tool batch; compression fires before the next LLM call.
  // Session event turn = kernel turn at compression time = next LLM metric turn.
  // Preceding LLM call is metric turn (kernelTurn - 1); post-compression snapshot is metric turn kernelTurn.
  for (const { event: e } of events) {
    if (e.kind !== "compressed") continue
    const kernelTurn = e.turn ?? 0
    const precedingLlmTurn = Math.max(0, kernelTurn - 1)
    const followingLlmTurn = kernelTurn
    const preceding = capturing.turnMetrics.find(m => m.turn === precedingLlmTurn)
    const following = capturing.turnMetrics.find(m => m.turn === followingLlmTurn)
    if (preceding) {
      preceding.compressionAction = e.action ?? "unknown"
      if (following?.contextSnapshot) preceding.postCompressionSnapshot = following.contextSnapshot
    } else if (following?.contextSnapshot) {
      // Provider errored before the preceding metric was recorded — attach to following turn.
      following.compressionAction = e.action ?? "unknown"
      following.postCompressionSnapshot = following.contextSnapshot
    }
  }

  const compressionEvents = events.filter(e => e.event.kind === "compressed")
  const peakInputTokens = Math.max(0, ...capturing.turnMetrics.map(m => m.inputTokens))

  const partial: HarnessResult = {
    id: cfg.id,
    passed: false,
    turnsUsed: capturing.turnMetrics.length,
    compressions: compressionEvents.length,
    compressionActions: compressionEvents
      .map(e => (e.event as { action?: string }).action ?? "unknown")
      .filter(Boolean),
    peakInputTokens,
    finalText,
    finalStatus,
    metrics: capturing.turnMetrics,
    events,
  }

  const { passed, failure } = cfg.validate(partial)
  return { ...partial, passed, failure }
}

// ── report formatter ──────────────────────────────────────────────────────────

export function printReport(results: HarnessResult[]): void {
  const pass = results.filter(r => r.passed).length
  const total = results.length

  console.log("\n═══════════════════════════════════════════════════")
  console.log(`  E2E Results: ${pass}/${total} passed`)
  console.log("═══════════════════════════════════════════════════")

  for (const r of results) {
    const icon = r.passed ? "✅" : "❌"
    const tokens = r.peakInputTokens > 0 ? `  peak_input=${r.peakInputTokens}` : ""
    const comp = r.compressions > 0 ? `  compress=${r.compressions}(${r.compressionActions.join(",")})` : ""
    console.log(`${icon} [${r.id}]  turns=${r.turnsUsed}  status=${r.finalStatus}${tokens}${comp}`)
    if (!r.passed && r.failure) console.log(`      ↳ ${r.failure}`)
  }
  console.log("═══════════════════════════════════════════════════\n")
}
