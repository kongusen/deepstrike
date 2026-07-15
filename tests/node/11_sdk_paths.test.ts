/**
 * 11_sdk_paths.test.ts — system_prompt, initialMemory, saveSession, knowledge.init(), frontmatter, AttemptLoop streaming
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import type { DoneEvent, TextDeltaEvent } from "@deepstrike/sdk"
import { AttemptLoop, LlmEvalJudge, RuntimeAttemptBody } from "@deepstrike/sdk/harness"
import type { AttemptLoopEvent } from "@deepstrike/sdk/harness"
import { makeAgent, makeProvider, collectEvents, text, MockDreamStore, MockKnowledgeSource, SKILL_DIR } from "./helpers.js"

// ─── system_prompt ────────────────────────────────────────────────────────

describe("system_prompt injection", () => {
  it("agent follows system_prompt instruction", { timeout: 60_000 }, async () => {
    const result = await makeAgent({
      systemPrompt: "You are a pirate. Always end every reply with 'Arrr!'",
    }).run("Say hello.")
    assert.ok(result.toLowerCase().includes("arrr"), `expected 'Arrr!' in: ${result}`)
  })
})

// ─── initialMemory ────────────────────────────────────────────────────────

describe("initialMemory injection", () => {
  it("agent can recall pre-seeded memory snippet", { timeout: 60_000 }, async () => {
    const result = await makeAgent({
      initialMemory: ["The user's favourite colour is chartreuse."],
    }).run("What is the user's favourite colour? Answer in one word.")
    assert.ok(result.toLowerCase().includes("chartreuse"), `expected 'chartreuse' in: ${result}`)
  })
})

// ─── saveSession ──────────────────────────────────────────────────────────

describe("DreamStore.saveSession() auto-call", () => {
  it("saveSession is called after run completes", { timeout: 60_000 }, async () => {
    const store = new MockDreamStore()
    await makeAgent({ dreamStore: store, agentId: "test-agent" }).run('Reply "ok".')
    assert.ok(store.savedSessions.length >= 1, "saveSession should have been called")
    assert.equal(store.savedSessions[0].agentId, "test-agent")
  })
})

// ─── KnowledgeSource.init() ───────────────────────────────────────────────

describe("KnowledgeSource.init() warmup", () => {
  it("init() is called before the first run", { timeout: 60_000 }, async () => {
    const ks = new MockKnowledgeSource(["DeepStrike is a Rust-kernel agent framework."])
    await makeAgent({ knowledgeSource: ks }).run('Reply "ok".')
    assert.ok(ks.initCalled >= 1, "init() should have been called")
  })

  it("init() is called exactly once per run", { timeout: 60_000 }, async () => {
    const ks = new MockKnowledgeSource([])
    const agent = makeAgent({ knowledgeSource: ks })
    await agent.run('Reply "ok".')
    assert.equal(ks.initCalled, 1)
  })
})

// ─── Frontmatter stripping ────────────────────────────────────────────────

describe("Skill frontmatter stripping", () => {
  it("skill content returned to LLM has no frontmatter", { timeout: 60_000 }, async () => {
    // The summarize skill fixture has frontmatter; if stripped correctly the LLM
    // will see only the body and produce a bullet-point summary.
    const events = await collectEvents(
      makeAgent({ skillDir: SKILL_DIR }).runStreaming(
        "Use the summarize skill on: 'Rust is fast, safe, and concurrent.' Then output the summary.",
      ),
    )
    const output = text(events)
    // If frontmatter leaked, the LLM would echo YAML keys like "name:" or "description:"
    assert.ok(!output.includes("name: summarize"), `frontmatter leaked into output: ${output}`)
    assert.ok(output.length > 0)
  })
})

// ─── AttemptLoop.stream() ─────────────────────────────────────────────────

describe("AttemptLoop.stream()", () => {
  it("emits token, supervising, and terminal events", { timeout: 90_000 }, async () => {
    const events: AttemptLoopEvent[] = []
    let result = ""
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(makeAgent().runner),
      judge: new LlmEvalJudge(makeProvider()),
      stop: { maxAttempts: 2 },
    })
    for await (const evt of loop.stream({
      goal: "What is 6 * 7? Output only the number.",
      criteria: [{ text: "Answer must be 42", required: true }],
    })) {
      events.push(evt)
      if (evt.type === "token") result += evt.text
    }
    assert.ok(result.length > 0, "should have token output")
    assert.ok(events.some(e => e.type === "judging"), "should emit judging")
    assert.ok(
      events.some(e => e.type === "completed"),
      "should terminate",
    )
  })

  it("verdict contains structured criterion details on done", { timeout: 90_000 }, async () => {
    let verdict: Extract<AttemptLoopEvent, { type: "completed" }>["outcome"]["verdict"]
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(makeAgent().runner),
      judge: new LlmEvalJudge(makeProvider()),
      stop: { maxAttempts: 2 },
    })
    for await (const evt of loop.stream({
      goal: "Output the number 99.",
      criteria: [
        { text: "Response must contain 99", required: true },
        { text: "Response should be concise", required: false, weight: 0.5 },
      ],
    })) {
      if (evt.type === "completed") verdict = evt.outcome.verdict
    }
    if (verdict) {
      assert.ok(typeof verdict.passed === "boolean")
      assert.ok(typeof verdict.overallScore === "number")
      assert.ok(Array.isArray(verdict.details))
    }
  })
})
