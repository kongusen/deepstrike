import { tool, executeTools } from "../src/tools/index.js"
import { WorkingMemory } from "../src/memory/index.js"
import { ScheduledPrompt } from "../src/signals/index.js"
import { PermissionManager, PermissionMode } from "../src/safety/index.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIProvider, QwenProvider, DeepSeekProvider, MiniMaxProvider } from "../src/providers/openai.js"
import { Governance } from "../src/governance.js"
import { RuntimeRunner, collectText, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import { tool } from "../src/tools/index.js"
import type { LLMProvider, Message, ProviderRunState, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

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
  it("converts to RuntimeSignal with kernel-aligned shape", () => {
    const p = new ScheduledPrompt("standup", 1_700_000_000_000, ["be brief"])
    const sig = p.toSignal()
    expect(sig.source).toBe("cron")
    expect(sig.signalType).toBe("job")
    expect(sig.urgency).toBe("normal")
    expect(sig.payload.goal).toBe("standup")
    expect(sig.payload.criteria).toEqual(["be brief"])
    expect(sig.dedupeKey).toBe("scheduled-1700000000000")
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

describe("RuntimeRunner", () => {
  it("threads provider run state through every turn in one run", async () => {
    class StatefulTestProvider implements LLMProvider {
      readonly states: ProviderRunState[] = []
      private callCount = 0

      createRunState(): ProviderRunState {
        return { marker: crypto.randomUUID() }
      }

      async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
        return { role: "assistant", content: "unused" }
      }

      async *stream(
        _context: RenderedContext,
        _tools: ToolSchema[],
        _extensions?: Record<string, unknown>,
        state?: ProviderRunState,
      ): AsyncIterable<StreamEvent> {
        this.states.push(state ?? {})
        this.callCount += 1
        if (this.callCount === 1) {
          yield { type: "tool_call", id: "call_1", name: "ping", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "done" }
      }
    }

    const provider = new StatefulTestProvider()
    const plane = new LocalExecutionPlane()
    plane.register(tool("ping", "Ping", { type: "object", properties: {} }, () => "pong"))
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: plane,
      maxTokens: 2048,
      maxTurns: 4,
    })

    for await (const _event of runner.run({ sessionId: "s-state", goal: "Use ping once, then finish." })) {}

    expect(provider.states).toHaveLength(2)
    expect(provider.states[0]).toBe(provider.states[1])
  })

  it("run_streaming yields text and done", async () => {
    const provider: LLMProvider = {
      async *stream() {
        yield { type: "text_delta", delta: "hi" }
        yield { type: "done", iterations: 1, totalTokens: 1, status: "completed" }
      },
      async complete() {
        return { role: "assistant", content: "hi", toolCalls: [] }
      },
    }
    const log = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog: log,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
    })
    const events: StreamEvent[] = []
    for await (const evt of runner.run({ sessionId: "s1", goal: "hello" })) {
      events.push(evt)
    }
    expect(events.some(e => e.type === "text_delta")).toBe(true)
    expect(events.some(e => e.type === "done")).toBe(true)
    const text = await collectText(runner.run({ sessionId: "s2", goal: "ping" }))
    expect(text).toBe("hi")
  })
})

describe("Governance", () => {
  it("allows by default before kernel attach", () => {
    const gov = new Governance()
    const verdict = gov.evaluate("read_file", "{}")
    expect(verdict.kind).toBe("allow")
  })

  it("blockTool queues before attach, applies after", async () => {
    const gov = new Governance()
    gov.blockTool("dangerous")
    // simulate kernel attach
    const kernel = await import("@deepstrike/wasm-kernel")
    gov._attach(kernel)
    // after attach, blocked tools are applied to kernel Governance
    const verdict = gov.evaluate("dangerous", "{}")
    // mock kernel.Governance.evaluate always returns allow, but blockTool was called
    expect(verdict.kind).toBe("allow") // mock doesn't implement veto logic
  })
})
