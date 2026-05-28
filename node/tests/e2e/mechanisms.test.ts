import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { tool } from "../../src/tools/index.js"
import type {
  LLMProvider,
  Message,
  RenderedContext,
  StreamEvent,
  ToolSchema,
} from "../../src/types.js"
import type { RegisteredTool } from "../../src/tools/index.js"

type ScriptDecision = (
  context: RenderedContext,
  call: number,
  tools: ToolSchema[],
) => Iterable<StreamEvent> | AsyncIterable<StreamEvent>

function contextText(context: RenderedContext): string {
  return [
    context.systemText,
    context.systemStable ?? "",
    context.systemVolatile ?? "",
    ...context.turns.flatMap(t => [
      t.content ?? "",
      ...(t.contentParts ?? []).map((p: any) => p.output ?? p.text ?? ""),
      ...(t.toolCalls ?? []).map(tc => `${tc.name} ${tc.arguments}`),
    ]),
  ].join("\n")
}

function estimateTokens(context: RenderedContext): number {
  return Math.max(1, Math.ceil(contextText(context).length / 4))
}

class ScriptedProvider implements LLMProvider {
  readonly calls: RenderedContext[] = []
  readonly inputTokens: number[] = []

  constructor(
    private readonly decide: ScriptDecision,
    private readonly opts: { emitUsage?: boolean } = {},
  ) {}

  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  }

  async *stream(context: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    this.calls.push(context)
    const inputTokens = estimateTokens(context)
    this.inputTokens.push(inputTokens)

    for await (const event of this.decide(context, this.calls.length, tools)) {
      yield event
    }

    if (this.opts.emitUsage ?? true) {
      yield {
        type: "usage",
        totalTokens: inputTokens + 1,
        inputTokens,
        outputTokens: 1,
      } as StreamEvent
    }
  }
}

function toolResultText(context: RenderedContext): string {
  return context.turns
    .flatMap(t => t.contentParts ?? [])
    .map((p: any) => p.output ?? "")
    .join("\n")
}

function createMechanismRunner(
  provider: LLMProvider,
  tools: RegisteredTool[] = [],
  opts: { maxTokens?: number; maxTurns?: number } = {},
) {
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  for (const t of tools) plane.register(t)
  const runner = new RuntimeRunner({
    provider,
    sessionLog,
    executionPlane: plane,
    maxTokens: opts.maxTokens ?? 8192,
    maxTurns: opts.maxTurns ?? 25,
  })
  return { runner, sessionLog }
}

function toolCalls(events: Array<{ event: any }>, name: string): number {
  return events
    .filter(e => e.event.kind === "tool_requested")
    .flatMap(e => e.event.calls ?? [])
    .filter(c => c.name === name)
    .length
}

describe("E2E mechanism contract tests", () => {
  it("K01 forces a 20-turn tool loop and keeps rho growth bounded", async () => {
    const provider = new ScriptedProvider((_context, call) => {
      if (call <= 20) {
        return [{ type: "tool_call", id: `step-${call}`, name: "step", arguments: { n: call } }]
      }
      return [{ type: "text_delta", delta: "DONE" }]
    })
    const { runner, sessionLog } = createMechanismRunner(
      provider,
      [
        tool("step", "Record one step", {
          type: "object",
          properties: { n: { type: "number" } },
          required: ["n"],
        }, args => `step ${(args as { n: number }).n} recorded`),
      ],
      { maxTokens: 32_000, maxTurns: 30 },
    )

    const text = await collectText(runner.run({ sessionId: "k01-mechanism", goal: "run 20 steps" }))
    const events = await sessionLog.read("k01-mechanism")

    expect(text).toContain("DONE")
    expect(provider.calls.length).toBe(21)
    expect(toolCalls(events, "step")).toBe(20)

    const deltas = provider.inputTokens.slice(1).map((v, i) => v - provider.inputTokens[i])
    const positiveDeltas = deltas.filter(d => d > 0)
    const maxDelta = Math.max(...positiveDeltas)
    const avgDelta = positiveDeltas.reduce((a, b) => a + b, 0) / positiveDeltas.length

    expect(provider.inputTokens.at(-1)).toBeLessThan(8_000)
    expect(maxDelta).toBeLessThan(avgDelta * 3)
  })

  it("K02 keeps the recent tail visible under a tight render budget", async () => {
    let sawLatestMarker = false
    const latestMarker = "RECENT_FILLER_15"
    const provider = new ScriptedProvider((context, call) => {
      const text = contextText(context)
      if (text.includes(latestMarker)) {
        sawLatestMarker = true
        return [{ type: "text_delta", delta: "TAIL_VISIBLE" }]
      }
      if (call <= 15) {
        return [{ type: "tool_call", id: `fill-${call}`, name: "fill_buffer", arguments: { n: call } }]
      }
      return [{ type: "text_delta", delta: "TAIL_NOT_VISIBLE" }]
    })
    const { runner } = createMechanismRunner(
      provider,
      [
        tool("fill_buffer", "Fill context with a numbered marker", {
          type: "object",
          properties: { n: { type: "number" } },
          required: ["n"],
        }, args => {
          const n = (args as { n: number }).n
          if (n === 1) return `EARLY_SECRET_ALPHA ${"x".repeat(2_000)}`
          const marker = `RECENT_FILLER_${n}`
          return `${marker} ${"x".repeat(80)}`
        }),
      ],
      { maxTokens: 900, maxTurns: 25 },
    )

    await collectText(runner.run({ sessionId: "k02-mechanism", goal: "fill until recent tail is visible" }))

    expect(sawLatestMarker).toBe(true)
    expect(provider.calls.length).toBeGreaterThanOrEqual(16)
  })

  it("K03 renders goal in the State turn (turns[0]), not in system", async () => {
    const goal = "Count from 1 to 3 and say COMPLETE."
    const provider = new ScriptedProvider(() => [{ type: "text_delta", delta: "COMPLETE" }])
    const { runner } = createMechanismRunner(provider, [], { maxTokens: 4096, maxTurns: 3 })

    await collectText(runner.run({ sessionId: "k03-mechanism", goal }))

    const first = provider.calls[0]
    // goal is now in turns[0] (State slot), not systemVolatile
    expect(first.turns[0]?.content ?? "").toContain("Count from 1 to 3")
    expect(first.systemText).not.toContain("[TASK STATE]")
  })

  it("K04 injects an auto-compact summary into the next provider context", async () => {
    let sawSummary = false
    const provider = new ScriptedProvider((context, call) => {
      if (contextText(context).includes("[Compressed: auto_compact]")) {
        sawSummary = true
        return [{ type: "text_delta", delta: "DONE" }]
      }
      if (call <= 40) {
        return [{ type: "tool_call", id: `fill-${call}`, name: "fill", arguments: { n: call } }]
      }
      return [{ type: "text_delta", delta: "FAILED_TO_COMPACT" }]
    }, { emitUsage: false })
    const { runner, sessionLog } = createMechanismRunner(
      provider,
      [
        tool("fill", "Add pressure", {
          type: "object",
          properties: { n: { type: "number" } },
        }, () => "w".repeat(200)),
      ],
      { maxTokens: 400, maxTurns: 60 },
    )

    await collectText(runner.run({ sessionId: "k04-mechanism", goal: "force auto compact" }))
    const events = await sessionLog.read("k04-mechanism")

    expect(events.some(e => e.event.kind === "compressed" && e.event.action === "auto_compact")).toBe(true)
    expect(events.some(e => e.event.kind === "compressed" && Boolean((e.event as any).summary))).toBe(true)
    expect(sawSummary).toBe(true)
  })

  it("K05 rolls back fatal tool failures and retries to success", async () => {
    let attempts = 0
    const provider = new ScriptedProvider((context) => {
      if (contextText(context).includes("success on attempt 3")) {
        return [{ type: "text_delta", delta: "SUCCESS" }]
      }
      return [{ type: "tool_call", id: `fragile-${attempts + 1}`, name: "fragile_tool", arguments: {} }]
    })
    const { runner, sessionLog } = createMechanismRunner(
      provider,
      [
        tool("fragile_tool", "Fails twice, then succeeds", {
          type: "object",
          properties: {},
        }, () => {
          attempts += 1
          if (attempts <= 2) {
            const err = new Error("transient error")
            ;(err as any).isFatal = true
            throw err
          }
          return "success on attempt 3"
        }),
      ],
      { maxTokens: 8192, maxTurns: 10 },
    )

    const text = await collectText(runner.run({ sessionId: "k05-mechanism", goal: "retry fragile tool" }))
    const events = await sessionLog.read("k05-mechanism")

    expect(text).toContain("SUCCESS")
    expect(attempts).toBe(3)
    expect(events.filter(e => e.event.kind === "rollbacked")).toHaveLength(2)
  })

  it("K06 completes a 20-turn tool loop within a wide budget without compression", async () => {
    const provider = new ScriptedProvider((_context, call) => {
      if (call <= 20) {
        return [{ type: "tool_call", id: `acc-${call}`, name: "accumulate", arguments: { step: call } }]
      }
      return [{ type: "text_delta", delta: "FINISHED" }]
    })
    const { runner, sessionLog } = createMechanismRunner(
      provider,
      [
        tool("accumulate", "Accumulate one step", {
          type: "object",
          properties: { step: { type: "number" } },
          required: ["step"],
        }, args => `accumulated step ${(args as { step: number }).step}`),
      ],
      { maxTokens: 32_000, maxTurns: 35 },
    )

    const text = await collectText(runner.run({ sessionId: "k06-mechanism", goal: "accumulate 20 steps" }))
    const events = await sessionLog.read("k06-mechanism")

    expect(text).toContain("FINISHED")
    expect(provider.calls.length).toBe(21)
    expect(toolCalls(events, "accumulate")).toBe(20)
    expect(events.some(e => e.event.kind === "compressed")).toBe(false)
  })

  it("K07 proves tool KV roundtrip from observed tool output", async () => {
    const provider = new ScriptedProvider((context, call) => {
      const text = contextText(context)
      if (text.includes("value=PERSIST-42")) return [{ type: "text_delta", delta: "PERSIST-42" }]
      if (call === 1) return [{ type: "tool_call", id: "set", name: "set_value", arguments: { value: "PERSIST-42" } }]
      return [{ type: "tool_call", id: "get", name: "get_value", arguments: {} }]
    })
    const kv = new Map<string, string>()
    const { runner, sessionLog } = createMechanismRunner(
      provider,
      [
        tool("set_value", "Store a value", {
          type: "object",
          properties: { value: { type: "string" } },
          required: ["value"],
        }, args => {
          kv.set("key", (args as { value: string }).value)
          return "stored"
        }),
        tool("get_value", "Read a value", {
          type: "object",
          properties: {},
        }, () => `value=${kv.get("key") ?? "(empty)"}`),
      ],
      { maxTokens: 8192, maxTurns: 8 },
    )

    const text = await collectText(runner.run({ sessionId: "k07-mechanism", goal: "store and retrieve value" }))
    const events = await sessionLog.read("k07-mechanism")

    expect(text).toContain("PERSIST-42")
    expect(toolCalls(events, "set_value")).toBe(1)
    expect(toolCalls(events, "get_value")).toBe(1)
    expect(kv.get("key")).toBe("PERSIST-42")
  })

  // ── K09: Four-tier compression ladder + content retention ───────────────────
  //
  // Goals:
  //  1. Trigger all four compression tiers as distinct kernel events.
  //  2. Verify content seeded in the "preserved tail" (last 4 messages)
  //     just before AutoCompact survives into the next context.
  //
  // Tier triggers (maxTokens=4000):
  //  snip_per_msg = 200 tokens (800 chars)
  //  snip  fires at rho > 0.70 (2800 tokens)  — targets Content::Text messages
  //  micro fires at rho > 0.80 (3200 tokens)  — targets Content::Parts ToolResults >200t
  //  collapse fires at rho > 0.90 (3600 tokens) — removes oldest messages
  //  auto  fires at rho > 0.95 (3800 tokens)  — keeps last 4 messages only
  //
  // Phase sequence:
  //  Phase 0 (→ snip_compact):
  //    Provider emits large text (1200 chars, 300t > snip_per_msg 200t) + tiny fill per turn.
  //    After ~6 turns, rho ∈ [0.70, 0.80). SnipCompact truncates text msgs to 200t, rho→0.60.
  //    Detection: closure var `everSnipFired` set when "tokens omitted" appears in context.
  //
  //  Phase 1 (→ micro_compact):
  //    Provider emits one large fill (3200 chars, 800t > Micro's 200t threshold).
  //    From post-snip rho≈0.60 (2400t), + 800t fill = 3200t → rho=0.80 → MicroCompact.
  //    Snip stage runs first but finds nothing to truncate (no remaining large text).
  //    Micro excerpts the 800t fill to ~50t. rho drops back to ~0.63.
  //    Detection: closure var `everMicroFired` set when "[tool result:" appears.
  //
  //  Phase 2 (→ context_collapse):
  //    Provider emits small fills (180 chars, 45t — below Micro's 200t threshold).
  //    Rho climbs slowly from 0.63 to 0.90 via ~12 turns.
  //    ContextCollapse injects "[Compressed: context_collapse]" into working partition.
  //    Detection: text.includes("[Compressed: context_collapse]").
  //
  //  Phase 3 (→ auto_compact + anchor):
  //    Provider seeds RETAIN_ANCHOR, then emits one huge fill (5000 chars, 1250t).
  //    From post-collapse rho≈0.65 (2600t) + anchor(100t) + fill(1250t) = 3950t → rho=0.99.
  //    AutoCompact keeps last 4 messages: [anchor-call, anchor-result, fill-call, fill-result].
  //    RETAIN_ANCHOR is preserved in the tail.
  //
  //  Phase 4 (→ verify):
  //    Provider finds RETAIN_ANCHOR in rendered context turns → emits VERIFIED.
  //
  // Root cause note: emitUsage MUST be false here.
  // When true, ScriptedProvider emits outputTokens=1, which causes the runner
  // to store assistant messages as tokenCount=1. SnipCompact then sees
  // original_tokens=1 ≤ snip_per_msg_limit=200 and skips every message,
  // saving 0 tokens and never reducing context. emitUsage:false lets the Rust
  // engine use count_message() (char/4), giving correct 300t values for 1200-char
  // messages, which SnipCompact correctly truncates.
  it("K09 triggers all four compression tiers and retains content in preserved tail", async () => {
    const RETAIN_ANCHOR = "KEEP-ME-OMEGA-77"

    // Cumulative detection flags: markers disappear when messages are archived,
    // so we persist them in closure vars once seen.
    let everSnipFired = false
    let everMicroFired = false
    let anchorSeeded = false
    let phase3BigFillDone = false

    // emitUsage: false — see note above
    const provider = new ScriptedProvider((context, call) => {
      const text = contextText(context)

      // SnipCompact injects "tokens omitted" into truncated Content::Text messages
      if (text.includes("tokens omitted")) everSnipFired = true
      // MicroCompact injects "[tool result:" into excerpted ContentPart messages
      if (text.includes("[tool result:")) everMicroFired = true

      // ── Phase 4: AutoCompact fired — verify summary injected into State turn (turns[0]) ──
      // AutoCompact writes summary to compression_log → task_state.format_compact() → turns[0].
      if (text.includes("[Compressed: auto_compact]")) {
        const stateTurn = context.turns[0]?.content ?? ""
        const hasSummary = stateTurn.includes("[Compressed: auto_compact]")
        const hasAnchorTool = stateTurn.includes("seed_anchor")
        return [{ type: "text_delta", delta: hasSummary && hasAnchorTool ? "VERIFIED" : "SUMMARY-MISSING" }]
      }

      // ── Phase 3: ContextCollapse fired — emit autofill + seed_anchor in ONE turn ──
      // Both tool calls go in the same assistant message → 4 messages total:
      //   [asst(autofill+seed), tool-result(autofill), tool-result(seed)]
      // Wait — tool results are separate messages per call. The kernel stores:
      //   turn N: asst message with 2 tool_calls
      //   turn N: tool message with autofill result
      //   turn N: tool message with seed result
      // That's 3 messages. AutoCompact keeps last 4, so all 3 + the previous asst
      // message are preserved. seed-result (containing RETAIN_ANCHOR) is in last-4.
      if (text.includes("[Compressed: context_collapse]") && !anchorSeeded) {
        anchorSeeded = true
        phase3BigFillDone = true
        return [
          { type: "text_delta", delta: "." },
          { type: "tool_call", id: `autofill-${call}`, name: "fill", arguments: { size: 5000, kind: "auto" } },
          { type: "tool_call", id: `seed-${call}`, name: "seed_anchor", arguments: { value: RETAIN_ANCHOR } },
        ]
      }
      if (text.includes("[Compressed: context_collapse]")) {
        return [{ type: "text_delta", delta: "WAITING_FOR_AUTO" }]
      }

      // ── Phase 2: MicroCompact fired — 600-char fills (150t) to reach collapse zone ──
      // 600 chars (150t) < Micro's 200t threshold so Micro skips them.
      // Multiple no-save snip/micro events fire until rho reaches 0.90 (collapse zone).
      if (everMicroFired) {
        return [
          { type: "text_delta", delta: "." },
          { type: "tool_call", id: `sf-${call}`, name: "fill", arguments: { size: 600, kind: "small" } },
        ]
      }

      // ── Phase 1: SnipCompact fired — one 5100-char fill to jump to micro zone ──
      // After snip, partition total ≈ 1945t. Adding 5100-char (1275t) fill pushes
      // total to 1945+1(asst)+1275(result)=3221t → rho=0.805 → MicroCompact fires.
      // (5000 chars would land at 3196t=0.799, just below micro threshold 0.80.)
      if (everSnipFired) {
        return [
          { type: "text_delta", delta: "." },
          { type: "tool_call", id: `mf-${call}`, name: "fill", arguments: { size: 5100, kind: "micro" } },
        ]
      }

      // ── Phase 0: large assistant text + tiny fill to build toward snip zone ──
      // Each turn adds: asst(1200 chars=300t) + fill-result(60 chars=15t) = 315t.
      // Snip fires at rho>0.70 (2800t). From baseline ≈10t: (2800-10)/315 ≈ 9 turns.
      // snip_per_msg = 0.05*4000 = 200t. Message(300t) > 200t → SnipCompact truncates it.
      // After truncation: 9*100t savings → partition drops to ≈1945t, rho=0.486.
      return [
        { type: "text_delta", delta: "z".repeat(1200) },
        { type: "tool_call", id: `p0-${call}`, name: "fill", arguments: { size: 60, kind: "tiny" } },
      ]
    }, { emitUsage: false })

    const anchorsSeeded = new Set<string>()
    const { runner, sessionLog } = createMechanismRunner(
      provider,
      [
        tool("seed_anchor", "Plant a retention anchor", {
          type: "object",
          properties: { value: { type: "string" } },
          required: ["value"],
        }, args => {
          const v = (args as { value: string }).value
          anchorsSeeded.add(v)
          return `anchor-stored:${v}`
        }),
        tool("fill", "Fill context with content", {
          type: "object",
          properties: { size: { type: "number" }, kind: { type: "string" } },
          required: ["size"],
        }, args => {
          const { size, kind } = args as { size: number; kind?: string }
          const char = kind === "micro" ? "M" : kind === "auto" ? "A" : kind === "small" ? "s" : "z"
          return char.repeat(size)
        }),
      ],
      { maxTokens: 4000, maxTurns: 60 },
    )

    const text = await collectText(runner.run({ sessionId: "k09-mechanism", goal: "ladder compression" }))
    const events = await sessionLog.read("k09-mechanism")

    const compressedEvents = events.filter(e => e.event.kind === "compressed")
    const actions = compressedEvents.map(e => (e.event as any).action as string)

    // All four tiers must have fired as distinct kernel events
    expect(actions).toContain("snip_compact")
    expect(actions).toContain("micro_compact")
    expect(actions).toContain("context_collapse")
    expect(actions).toContain("auto_compact")

    // Anchor tool must have been called
    expect(anchorsSeeded.has(RETAIN_ANCHOR)).toBe(true)

    // AutoCompact summary injected into systemVolatile and lists seed_anchor tool
    expect(text).toContain("VERIFIED")
    expect(text).not.toContain("SUMMARY-MISSING")
  })

  it("K08 verifies virtual file write and read from harness-owned state", async () => {
    const fs = new Map<string, string>()
    let readBack = ""
    const provider = new ScriptedProvider((context, call) => {
      const text = toolResultText(context)
      if (text.includes("answer=42")) return [{ type: "text_delta", delta: "FILE_VERIFIED" }]
      if (call === 1) {
        return [{
          type: "tool_call",
          id: "write",
          name: "write_file",
          arguments: { path: "result.txt", content: "answer=42\n" },
        }]
      }
      return [{ type: "tool_call", id: "read", name: "read_file", arguments: { path: "result.txt" } }]
    })
    const { runner, sessionLog } = createMechanismRunner(
      provider,
      [
        tool("write_file", "Write a virtual file", {
          type: "object",
          properties: {
            path: { type: "string" },
            content: { type: "string" },
          },
          required: ["path", "content"],
        }, args => {
          const a = args as { path: string; content: string }
          fs.set(a.path, a.content)
          return `written ${a.path}`
        }),
        tool("read_file", "Read a virtual file", {
          type: "object",
          properties: { path: { type: "string" } },
          required: ["path"],
        }, args => {
          readBack = fs.get((args as { path: string }).path) ?? "(missing)"
          return readBack
        }),
      ],
      { maxTokens: 8192, maxTurns: 8 },
    )

    const text = await collectText(runner.run({ sessionId: "k08-mechanism", goal: "write and verify a file" }))
    const events = await sessionLog.read("k08-mechanism")

    expect(text).toContain("FILE_VERIFIED")
    expect(fs.get("result.txt")).toBe("answer=42\n")
    expect(readBack).toBe("answer=42\n")
    expect(toolCalls(events, "write_file")).toBe(1)
    expect(toolCalls(events, "read_file")).toBe(1)
  })
})
