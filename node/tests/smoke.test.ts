import { CircuitBreaker, normalizeToolCall } from "../src/providers/base.js"
import { tool, executeTools, readFile } from "../src/tools/index.js"
import { WorkingMemory } from "../src/memory/working.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIChatProvider, OpenAIProvider } from "../src/providers/openai.js"
import { QwenProvider } from "../src/providers/qwen.js"
import { DeepSeekProvider } from "../src/providers/deepseek.js"
import { KimiProvider } from "../src/providers/kimi.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import { MiniMaxAnthropicProvider } from "../src/providers/minimax.js"
import { PermissionManager, PermissionMode } from "../src/safety/permissions.js"
import { ScheduledPrompt } from "../src/signals/scheduled.js"

describe("CircuitBreaker", () => {
  it("starts closed", () => {
    const cb = new CircuitBreaker()
    expect(cb.isOpen()).toBe(false)
  })

  it("opens after threshold failures", () => {
    const cb = new CircuitBreaker(3, 60_000)
    cb.recordFailure(); cb.recordFailure(); cb.recordFailure()
    expect(cb.isOpen()).toBe(true)
  })

  it("resets on success", () => {
    const cb = new CircuitBreaker(2, 60_000)
    cb.recordFailure(); cb.recordFailure()
    cb.recordSuccess()
    expect(cb.isOpen()).toBe(false)
  })
})

describe("normalizeToolCall", () => {
  it("returns null for empty name", () => {
    expect(normalizeToolCall("id", "", {})).toBeNull()
  })

  it("parses string arguments", () => {
    const tc = normalizeToolCall("id1", "my_tool", '{"x":1}')
    expect(tc).not.toBeNull()
    expect(JSON.parse(tc!.arguments)).toEqual({ x: 1 })
  })

  it("accepts object arguments", () => {
    const tc = normalizeToolCall("id2", "my_tool", { y: 2 })
    expect(JSON.parse(tc!.arguments)).toEqual({ y: 2 })
  })
})

describe("tool + executeTools", () => {
  it("registers and executes a tool", async () => {
    const add = tool("add", "Add two numbers", {
      type: "object",
      properties: { a: { type: "number" }, b: { type: "number" } },
      required: ["a", "b"],
    }, ({ a, b }) => String(Number(a) + Number(b)))

    const registry = new Map([["add", add]])
    const results = await executeTools([{ id: "c1", name: "add", arguments: '{"a":2,"b":3}' }], registry)
    expect(results[0].output).toBe("5")
    expect(results[0].isError).toBe(false)
  })

  it("returns error for unknown tool", async () => {
    const results = await executeTools([{ id: "c2", name: "missing", arguments: "{}" }], new Map())
    expect(results[0].isError).toBe(true)
  })
})

describe("readFile tool", () => {
  it("has correct schema", () => {
    expect(readFile.schema.name).toBe("read_file")
    const params = JSON.parse(readFile.schema.parameters)
    expect(params.required).toContain("path")
  })
})

describe("WorkingMemory", () => {
  it("stores and retrieves values", () => {
    const mem = new WorkingMemory()
    mem.set("key", 42)
    expect(mem.get("key")).toBe(42)
  })

  it("returns default for missing key", () => {
    const mem = new WorkingMemory()
    expect(mem.get("missing", "default")).toBe("default")
  })

  it("clears all entries", () => {
    const mem = new WorkingMemory()
    mem.set("a", 1)
    mem.clear()
    expect(mem.has("a")).toBe(false)
  })
})

describe("PermissionManager", () => {
  it("supports approval-gated grants", () => {
    const pm = new PermissionManager(PermissionMode.DEFAULT)
    pm.grantWithApproval("db", "write", "Needs review")
    expect(pm.evaluate("db", "write")).toEqual(expect.objectContaining({
      allowed: false,
      requiresApproval: true,
      reason: "Needs review",
    }))
  })
})

describe("ScheduledPrompt", () => {
  it("preserves kernel routing metadata", () => {
    const sig = new ScheduledPrompt("standup", 123).toSignal()
    expect(sig).toMatchObject({
      source: "cron",
      signalType: "job",
      urgency: "normal",
      dedupeKey: "cron:standup:123",
    })
  })
})

describe("Provider instantiation", () => {
  it("AnthropicProvider constructs", () => {
    const p = new AnthropicProvider("test-key")
    expect(p).toBeDefined()
  })

  it("OpenAIProvider constructs", () => {
    const p = new OpenAIProvider("test-key")
    expect(p).toBeDefined()
  })

  it("OpenAIChatProvider and OpenAIResponsesProvider construct as separate native paths", () => {
    expect(new OpenAIChatProvider("test-key")).toBeInstanceOf(OpenAIChatProvider)
    expect(new OpenAIProvider("test-key")).toBeInstanceOf(OpenAIChatProvider)
    expect(new OpenAIResponsesProvider("test-key")).toBeInstanceOf(OpenAIResponsesProvider)
  })

  it("OpenAI-compatible providers construct beside browser-like editor globals and restore them", () => {
    const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window")
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      enumerable: true,
      writable: true,
      value: { document: {} },
    })

    try {
      const providers = [OpenAIProvider, OpenAIChatProvider, QwenProvider, DeepSeekProvider, MiniMaxAnthropicProvider, KimiProvider]
      for (const Provider of providers) expect(new Provider("test-key")).toBeDefined()
      expect((globalThis as typeof globalThis & { window?: unknown }).window).toEqual({ document: {} })
    } finally {
      if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow)
      else Reflect.deleteProperty(globalThis, "window")
    }
  })

  it("QwenProvider constructs", () => {
    expect(new QwenProvider("test-key")).toBeDefined()
  })

  it("DeepSeekProvider constructs", () => {
    expect(new DeepSeekProvider("test-key")).toBeDefined()
  })

  it("MiniMaxAnthropicProvider constructs", () => {
    expect(new MiniMaxAnthropicProvider("test-key")).toBeDefined()
  })
})
