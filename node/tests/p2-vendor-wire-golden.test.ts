// P2 wire golden master: locks the exact request body the reasoning OpenAI-chat
// vendors send, so the Template-Method collapse onto OpenAIChatProvider stays
// byte-for-byte on the wire. The load-bearing invariant (per vendor docs) is
// that `prompt_cache_key` is NOT sent — DeepSeek hard-fails 400 on unknown body
// params, and none of these vendors need it (all auto prefix-cache).
import { DeepSeekProvider } from "../src/providers/deepseek.js"
import { MiniMaxOpenAIProvider } from "../src/providers/minimax.js"
import { QwenProvider } from "../src/providers/qwen.js"
import type { RenderedContext } from "../src/types.js"

function captureComplete(provider: unknown): { reqs: Record<string, unknown>[] } {
  const cap = { reqs: [] as Record<string, unknown>[] }
  ;(provider as { client: { chat: { completions: { create(req: Record<string, unknown>): Promise<Record<string, unknown>> } } } }).client = {
    chat: { completions: {
      async create(req: Record<string, unknown>) {
        cap.reqs.push(req)
        return { choices: [{ message: { content: "ok", tool_calls: [] } }], usage: { total_tokens: 5, completion_tokens: 3 } }
      },
    } },
  }
  return cap
}

const CTX: RenderedContext = { systemText: "sys", turns: [{ role: "user", content: "hi" }] }

describe("P2 vendor wire golden — request body", () => {
  it("DeepSeek sends reasoning_effort + extra_body.thinking and NO prompt_cache_key (default)", async () => {
    const p = new DeepSeekProvider("test-key", "deepseek-v4-flash")
    const cap = captureComplete(p)
    await p.complete(CTX, [])
    const req = cap.reqs[0]
    expect(req).not.toHaveProperty("prompt_cache_key")
    expect(req.reasoning_effort).toBe("high")
    expect(req.extra_body).toEqual({ thinking: { type: "enabled" } })
    expect(req.model).toBe("deepseek-v4-flash")
  })

  it("DeepSeek honors reasoningEffort=max and thinking=false", async () => {
    const p = new DeepSeekProvider("test-key", "deepseek-v4-flash")
    const cap = captureComplete(p)
    await p.complete(CTX, [], { reasoningEffort: "max", thinking: false })
    const req = cap.reqs[0]
    expect(req.reasoning_effort).toBe("max")
    expect(req.extra_body).toEqual({ thinking: { type: "disabled" } })
    expect(req).not.toHaveProperty("prompt_cache_key")
  })

  it("MiniMax sends reasoning_split:true and NO prompt_cache_key (default)", async () => {
    const p = new MiniMaxOpenAIProvider("test-key", "MiniMax-M2.7")
    const cap = captureComplete(p)
    await p.complete(CTX, [])
    const req = cap.reqs[0]
    expect(req).not.toHaveProperty("prompt_cache_key")
    expect(req.reasoning_split).toBe(true)
    expect(req.model).toBe("MiniMax-M2.7")
  })

  it("MiniMax honors reasoning_split:false", async () => {
    const p = new MiniMaxOpenAIProvider("test-key", "MiniMax-M2.7")
    const cap = captureComplete(p)
    await p.complete(CTX, [], { reasoning_split: false })
    const req = cap.reqs[0]
    expect(req.reasoning_split).toBe(false)
    expect(req).not.toHaveProperty("prompt_cache_key")
  })

  it("Qwen sends NO extra_body and NO prompt_cache_key by default", async () => {
    const p = new QwenProvider("test-key", "qwen3.6-plus")
    const cap = captureComplete(p)
    await p.complete(CTX, [])
    const req = cap.reqs[0]
    expect(req).not.toHaveProperty("prompt_cache_key")
    expect(req).not.toHaveProperty("extra_body")
    expect(req.model).toBe("qwen3.6-plus")
  })

  it("Qwen opts into thinking via extra_body and strips the control keys from the wire", async () => {
    const p = new QwenProvider("test-key", "qwen3.6-plus")
    const cap = captureComplete(p)
    await p.complete(CTX, [], { enableThinking: true, thinkingBudget: 256, foo: "keep" })
    const req = cap.reqs[0]
    expect(req.extra_body).toEqual({ enable_thinking: true, thinking_budget: 256 })
    expect(req).not.toHaveProperty("prompt_cache_key")
    expect(req).not.toHaveProperty("enableThinking")
    expect(req).not.toHaveProperty("thinkingBudget")
    expect(req.foo).toBe("keep")
  })
})
