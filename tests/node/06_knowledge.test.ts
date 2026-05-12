/**
 * 06_knowledge.test.ts — KnowledgeSource (mock) + agent with knowledge
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { MockKnowledgeSource, makeAgent } from "./helpers.js"

describe("MockKnowledgeSource", () => {
  it("retrieve returns all snippets when topK >= count", async () => {
    const ks = new MockKnowledgeSource(["A", "B", "C"])
    assert.deepEqual(await ks.retrieve("q", 10), ["A", "B", "C"])
  })

  it("retrieve respects topK limit", async () => {
    const ks = new MockKnowledgeSource(["a", "b", "c", "d"])
    assert.equal((await ks.retrieve("q", 2)).length, 2)
  })

  it("retrieve returns [] for empty source", async () => {
    assert.deepEqual(await new MockKnowledgeSource([]).retrieve("q"), [])
  })
})

describe("Agent with knowledgeSource (real API)", () => {
  it("knowledge snippets influence the answer", { timeout: 60_000 }, async () => {
    const ks = new MockKnowledgeSource([
      "DeepStrike supports: OpenAI, Anthropic, Qwen, DeepSeek, MiniMax, Kimi, Ollama.",
    ])
    const result = await makeAgent({ knowledgeSource: ks }).run(
      "List at least two LLM providers that DeepStrike supports.",
    )
    const lower = result.toLowerCase()
    const found = ["openai", "anthropic", "qwen", "deepseek", "minimax", "kimi", "ollama"]
      .filter(p => lower.includes(p))
    assert.ok(found.length >= 2, `expected ≥2 providers, got: ${result}`)
  })

  it("empty knowledge source doesn't break the agent", { timeout: 60_000 }, async () => {
    const result = await makeAgent({ knowledgeSource: new MockKnowledgeSource([]) })
      .run('Reply with just "ok".')
    assert.ok(result.toLowerCase().includes("ok"))
  })
})
