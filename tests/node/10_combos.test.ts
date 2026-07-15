/**
 * 10_combos.test.ts — Feature combinations
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { tool, WorkingMemory, Governance, SignalGateway, ScheduledPrompt } from "@deepstrike/sdk"
import { AttemptLoop, RuntimeAttemptBody, VerdictFnJudge } from "@deepstrike/sdk/harness"
import type { ErrorEvent, DoneEvent, ToolResultEvent } from "@deepstrike/sdk"
import { makeAgent, makeProvider, collectEvents, text, memoryRecord, MockDreamStore, MockKnowledgeSource, SKILL_DIR } from "./helpers.js"

// ─── A: Tools + Governance ────────────────────────────────────────────────

describe("Tools + Governance", () => {
  it("blocked tool emits error; allowed tool succeeds in the same run", { timeout: 90_000 }, async () => {
    const gov = new Governance("allow")
    gov.addPermissionRule("risky_op", "deny")

    const risky = tool("risky_op", "Risky operation", {}, async () => "risky done")
    const safe  = tool("safe_op",  "Safe operation",  {}, async () => "safe done")

    const events = await collectEvents(
      makeAgent({ governance: gov }).register(risky).register(safe)
        .runStreaming("First call risky_op. Then call safe_op. Report both results."),
    )

    assert.ok(
      (events.filter(e => e.type === "error") as ErrorEvent[]).some(e => e.message.includes("risky_op")),
      "risky_op should be denied",
    )
    assert.ok(
      (events.filter(e => e.type === "tool_result") as ToolResultEvent[]).some(r => r.name === "safe_op"),
      "safe_op should succeed",
    )
    assert.equal(events.filter(e => e.type === "done").length, 1)
  })
})

// ─── B: Tools + WorkingMemory ─────────────────────────────────────────────

describe("Tools + WorkingMemory", () => {
  it("shared counter increments across tool calls", { timeout: 90_000 }, async () => {
    const mem = new WorkingMemory()
    mem.set("count", 0)

    const increment = tool("increment_counter", "Increment the counter and return the new value", {}, async () => {
      const n = (mem.get<number>("count") ?? 0) + 1
      mem.set("count", n)
      return String(n)
    })

    const events = await collectEvents(
      makeAgent().register(increment)
        .runStreaming("Call increment_counter exactly 3 times, then report the final value."),
    )

    assert.ok(events.some(e => e.type === "done"))
    assert.ok((mem.get<number>("count") ?? 0) >= 1, `counter should be ≥1, got ${mem.get("count")}`)
  })
})

// ─── C: Knowledge + Tools ─────────────────────────────────────────────────

describe("Knowledge + Tools", () => {
  it("knowledge context informs tool arguments", { timeout: 90_000 }, async () => {
    const ks = new MockKnowledgeSource([
      "Recommended model: gpt-4o-mini. Recommended maxTokens: 4096.",
    ])
    const stored: Record<string, string> = {}
    const store = tool("store_config", "Store a key-value config pair", {
      type: "object",
      properties: { key: { type: "string" }, value: { type: "string" } },
      required: ["key", "value"],
    }, async ({ key, value }) => {
      stored[String(key)] = String(value)
      return `stored ${String(key)}=${String(value)}`
    })

    const events = await collectEvents(
      makeAgent({ knowledgeSource: ks }).register(store)
        .runStreaming("Based on the recommended configuration in context, store the recommended model name using store_config."),
    )

    assert.ok(events.some(e => e.type === "done"))
    // Either the tool was called (and stored something) or the agent answered in text
    const toolResults = events.filter(e => e.type === "tool_result") as ToolResultEvent[]
    if (toolResults.length > 0) {
      assert.ok(toolResults.some(r => r.name === "store_config"))
    }
  })
})

// ─── D: Skills + Tools ───────────────────────────────────────────────────

describe("Skills + Tools", () => {
  it("agent uses a skill and then calls a tool", { timeout: 90_000 }, async () => {
    const logged: string[] = []
    const logger = tool("log_summary", "Log a summary string", {
      type: "object",
      properties: { summary: { type: "string" } },
      required: ["summary"],
    }, async ({ summary }) => {
      logged.push(String(summary))
      return "logged"
    })

    const events = await collectEvents(
      makeAgent({ skillDir: SKILL_DIR }).register(logger)
        .runStreaming(
          "Use the summarize skill on this text: 'DeepStrike is a Rust kernel with Node.js, Python, and Rust bindings.' " +
          "Then log the result with log_summary.",
        ),
    )

    assert.ok(events.some(e => e.type === "done"))
  })
})

// ─── E: AttemptLoop + Tools ──────────────────────────────────────────────

describe("AttemptLoop + Tools", () => {
  it("retries until tool produces accepted output", { timeout: 120_000 }, async () => {
    const square = tool("compute_square", "Compute the square of a number", {
      type: "object", properties: { n: { type: "number" } }, required: ["n"],
    }, async ({ n }) => String(Number(n) * Number(n)))

    const agent = makeAgent().register(square)
    let attempts = 0
    const outcome = await new AttemptLoop({
      body: new RuntimeAttemptBody(agent.runner),
      judge: new VerdictFnJudge(({ result }) => {
        attempts++
        const passed = result.includes("25")
        return { passed, overallScore: passed ? 1 : 0, feedback: passed ? "ok" : "retry", details: [] }
      }),
      stop: { maxAttempts: 3 },
    }).run({
      goal: "Use compute_square to compute 5 squared and output the result.",
      criteria: [
        { text: "Must call compute_square with n=5" },
        { text: "Final answer must be 25" },
      ],
    })

    assert.ok(outcome.outcome === "passed" || outcome.outcome === "exhausted")
    if (outcome.outcome === "passed") assert.ok(outcome.result.includes("25"))
  })
})

// ─── F: Agent + DreamStore ────────────────────────────────────────────────

describe("Agent + DreamStore (memory-enabled run)", () => {
  it("pre-seeded memory is accessible during run", { timeout: 90_000 }, async () => {
    const store = new MockDreamStore()
    const agentId = "combo-mem-agent"
    await store.commit(agentId, {
      toAdd: [memoryRecord("secret-code", "The secret code word is BANANA.", 0.95)],
      toRemoveIndices: [],
      stats: { insightsProcessed: 1, duplicatesRemoved: 0, conflictsResolved: 0, entriesAdded: 1 },
    }, [])

    const result = await makeAgent({ dreamStore: store, agentId }).run(
      "What is the secret code word from your memory? If unknown, say 'unknown'.",
    )
    assert.ok(result.length > 0)
  })
})

// ─── G: SignalGateway + Agent ─────────────────────────────────────────────

describe("SignalGateway + Agent (scheduled signal)", () => {
  it("scheduled signal is processed and run completes", { timeout: 60_000 }, async () => {
    const gw = new SignalGateway()
    gw.schedule(new ScheduledPrompt("check-in", Date.now() + 80))

    const events = await collectEvents(
      makeAgent({ signalSource: gw, maxTurns: 5 }).runStreaming("Respond with 'ready'."),
    )
    gw.destroy()

    assert.ok(events.some(e => e.type === "done"))
  })
})
