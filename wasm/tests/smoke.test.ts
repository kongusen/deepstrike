import { tool, executeTools } from "../src/tools/index.js"
import { WorkingMemory } from "../src/memory/index.js"
import { ScheduledPrompt } from "../src/signals/index.js"
import { PermissionManager, PermissionMode } from "../src/safety/index.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIProvider, QwenProvider, DeepSeekProvider, MiniMaxProvider } from "../src/providers/openai.js"
import { Agent } from "../src/agent.js"

describe("tool + executeTools", () => {
  const add = tool("add", "Add two numbers.", {
    type: "object", properties: { x: { type: "number" }, y: { type: "number" } }, required: ["x", "y"],
  }, async ({ x, y }) => String((x as number) + (y as number)))

  it("registers and executes a tool", async () => {
    const registry = new Map([["add", add]])
    const results = await executeTools([{ id: "1", name: "add", arguments: '{"x":2,"y":3}' }], registry)
    expect(results[0].output).toBe("5")
    expect(results[0].isError).toBe(false)
  })

  it("returns error for unknown tool", async () => {
    const results = await executeTools([{ id: "1", name: "nope", arguments: "{}" }], new Map())
    expect(results[0].isError).toBe(true)
  })
})

describe("WorkingMemory", () => {
  it("stores and retrieves values", () => {
    const mem = new WorkingMemory()
    mem.set("k", 42)
    expect(mem.get("k")).toBe(42)
  })

  it("returns default for missing key", () => {
    const mem = new WorkingMemory()
    expect(mem.get("missing", "default")).toBe("default")
  })

  it("clears all entries", () => {
    const mem = new WorkingMemory()
    mem.set("a", 1)
    mem.clear()
    expect(mem.get("a")).toBeUndefined()
  })
})

describe("ScheduledPrompt", () => {
  it("converts to RuntimeSignal", () => {
    const p = new ScheduledPrompt("standup", 1_700_000_000_000, ["be brief"])
    const sig = p.toSignal()
    expect(sig.kind).toBe("scheduled")
    expect(sig.payload.goal).toBe("standup")
    expect(sig.payload.criteria).toEqual(["be brief"])
  })
})

describe("PermissionManager", () => {
  it("grants and evaluates", () => {
    const pm = new PermissionManager(PermissionMode.DEFAULT)
    pm.grant("fs", "read")
    expect(pm.evaluate("fs", "read").allowed).toBe(true)
    expect(pm.evaluate("fs", "write").allowed).toBe(false)
  })

  it("AUTO mode allows all", () => {
    const pm = new PermissionManager(PermissionMode.AUTO)
    expect(pm.evaluate("bash", "execute").allowed).toBe(true)
  })

  it("PLAN mode blocks all", () => {
    const pm = new PermissionManager(PermissionMode.PLAN)
    pm.grant("fs", "*")
    expect(pm.evaluate("fs", "read").allowed).toBe(false)
  })

  it("wildcard grant", () => {
    const pm = new PermissionManager(PermissionMode.DEFAULT)
    pm.grant("fs", "*")
    expect(pm.evaluate("fs", "anything").allowed).toBe(true)
  })
})

describe("Provider instantiation", () => {
  it("AnthropicProvider constructs", () => {
    expect(() => new AnthropicProvider("sk-test")).not.toThrow()
  })
  it("OpenAIProvider constructs", () => {
    expect(() => new OpenAIProvider("sk-test")).not.toThrow()
  })
  it("QwenProvider constructs", () => {
    expect(() => new QwenProvider("sk-test")).not.toThrow()
  })
  it("DeepSeekProvider constructs", () => {
    expect(() => new DeepSeekProvider("sk-test")).not.toThrow()
  })
  it("MiniMaxProvider constructs", () => {
    expect(() => new MiniMaxProvider("sk-test")).not.toThrow()
  })
})

describe("Agent (mock kernel)", () => {
  it("run() returns done string", async () => {
    const provider = {
      async *stream() { yield { type: "text_delta", delta: "hello" } },
    }
    const agent = new Agent(provider, { maxTokens: 4096, maxTurns: 5 })
    const result = await agent.run("test goal")
    expect(result).toContain("done")
  })

  it("register and blockTool", () => {
    const provider = { async *stream() {} }
    const agent = new Agent(provider, { maxTokens: 4096 })
    const t = tool("t", "d", {}, async () => "ok")
    agent.register(t)
    agent.blockTool("t")
    agent.unregister("t")
  })
})
