import { CircuitBreaker, normalizeToolCall } from "../src/providers/base.js"
import { tool, executeTools, readFile } from "../src/tools/index.js"
import { WorkingMemory } from "../src/memory/working.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIProvider, QwenProvider, DeepSeekProvider, MiniMaxProvider } from "../src/providers/openai.js"

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

describe("Provider instantiation", () => {
  it("AnthropicProvider constructs", () => {
    const p = new AnthropicProvider("test-key")
    expect(p).toBeDefined()
  })

  it("OpenAIProvider constructs", () => {
    const p = new OpenAIProvider("test-key")
    expect(p).toBeDefined()
  })

  it("QwenProvider constructs", () => {
    expect(new QwenProvider("test-key")).toBeDefined()
  })

  it("DeepSeekProvider constructs", () => {
    expect(new DeepSeekProvider("test-key")).toBeDefined()
  })

  it("MiniMaxProvider constructs", () => {
    expect(new MiniMaxProvider("test-key")).toBeDefined()
  })
})
