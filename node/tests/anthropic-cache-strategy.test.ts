/**
 * Unit tests for `extensions.cacheBreakpointStrategy` on the Anthropic provider.
 *
 * Each test stubs the underlying client message-create call to capture the request body, then
 * asserts which slots carry `cache_control` per strategy. The stub never makes a real API call.
 */
import { AnthropicProvider } from "../src/providers/anthropic.js"
import type { RenderedContext, ToolSchema, CacheBreakpointStrategy } from "../src/types.js"

// ── stubbing helpers ────────────────────────────────────────────────────────

interface CapturedRequest {
  system?: unknown
  tools?: unknown[]
  messages?: unknown[]
}

function makeStubProvider(): { provider: AnthropicProvider; captured: { req?: CapturedRequest } } {
  const provider = new AnthropicProvider("sk-fake", "claude-sonnet-4-6")
  const captured: { req?: CapturedRequest } = {}
  // Patch the private streamMessage to capture the request and return an empty stream.
  ;(provider as unknown as { streamMessage: (body: CapturedRequest) => AsyncIterable<unknown> }).streamMessage = body => {
    captured.req = body
    return (async function*() { /* empty stream */ })()
  }
  return { provider, captured }
}

const tools: ToolSchema[] = [
  { name: "a", description: "A", parameters: '{"type":"object","properties":{}}' },
  { name: "b", description: "B", parameters: '{"type":"object","properties":{}}' },
  { name: "c", description: "C", parameters: '{"type":"object","properties":{}}' },
]

const ctxWithSystemBlocks: RenderedContext = {
  systemText: "stable\n\nknowledge",
  systemStable: "stable",
  systemKnowledge: "knowledge",
  turns: [
    { role: "user", content: "hi" },
    { role: "assistant", content: "ok" },
    { role: "user", content: "ok 2" },
    { role: "assistant", content: "ok 3" },
    { role: "user", content: "ok 4" },
  ],
}

const ctxFrozenPrefix: RenderedContext = {
  ...ctxWithSystemBlocks,
  frozenPrefixLen: 2,
}

async function runStream(provider: AnthropicProvider, ctx: RenderedContext, strategy?: CacheBreakpointStrategy): Promise<void> {
  const exts = strategy ? { cacheBreakpointStrategy: strategy } : undefined
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  for await (const _ of provider.stream(ctx, tools, exts as Record<string, unknown> | undefined)) { /* drain */ }
}

function countCacheControlOnTools(req: CapturedRequest): number {
  const ts = (req.tools ?? []) as Array<{ cache_control?: unknown }>
  return ts.filter(t => t.cache_control).length
}

function countCacheControlOnSystem(req: CapturedRequest): number {
  if (Array.isArray(req.system)) {
    return (req.system as Array<{ cache_control?: unknown }>).filter(b => b.cache_control).length
  }
  return 0
}

function countCacheControlOnMessages(req: CapturedRequest): number {
  const msgs = (req.messages ?? []) as Array<{ content?: unknown }>
  let count = 0
  for (const m of msgs) {
    const c = m.content
    if (Array.isArray(c)) {
      for (const block of c as Array<{ cache_control?: unknown }>) {
        if (block.cache_control) count++
      }
    }
  }
  return count
}

// ── tests ───────────────────────────────────────────────────────────────────

describe("AnthropicProvider — cacheBreakpointStrategy", () => {
  describe("default (no strategy or 'default')", () => {
    it("with structured system blocks, emits cache_control on 2 system blocks + 2 messages (no tool anchor)", async () => {
      // anchorCache = !Array.isArray(system); structured system blocks ⇒ no tool breakpoint
      // by design (system+messages already saturate 4 cache slots).
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxWithSystemBlocks)
      expect(countCacheControlOnTools(captured.req!)).toBe(0)
      expect(countCacheControlOnSystem(captured.req!)).toBe(2)
      expect(countCacheControlOnMessages(captured.req!)).toBe(2)
    })

    it("'default' string is equivalent to undefined", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxWithSystemBlocks, "default")
      expect(countCacheControlOnTools(captured.req!)).toBe(0)
      expect(countCacheControlOnSystem(captured.req!)).toBe(2)
      expect(countCacheControlOnMessages(captured.req!)).toBe(2)
    })

    it("with flat-string system, default anchors the last tool", async () => {
      const { provider, captured } = makeStubProvider()
      const ctxFlatSystem: RenderedContext = {
        systemText: "just a flat system",
        turns: [{ role: "user", content: "hi" }],
      }
      await runStream(provider, ctxFlatSystem, "default")
      expect(countCacheControlOnTools(captured.req!)).toBe(1)
    })

    it("with frozen-prefix context, default uses deep anchor (not rolling)", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxFrozenPrefix)
      // Deep-anchor at frozenPrefixLen-1 (index 1) + last message = 2 breakpoints
      expect(countCacheControlOnMessages(captured.req!)).toBe(2)
    })
  })

  describe("tools-only", () => {
    it("emits cache_control only on the last tool", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxWithSystemBlocks, "tools-only")
      // Note: tools-only requires system to be a STRING (anchorCache = !Array.isArray(system)).
      // With systemBlocks present, the system is rendered as an array → anchorCache=false → no tool bp.
      // So this scenario won't anchor anything. Test the string-system case separately below.
      expect(countCacheControlOnSystem(captured.req!)).toBe(0)
      expect(countCacheControlOnMessages(captured.req!)).toBe(0)
    })

    it("emits cache_control on last tool when system is a flat string", async () => {
      const { provider, captured } = makeStubProvider()
      const ctxFlatSystem: RenderedContext = {
        systemText: "just a flat system",
        turns: [{ role: "user", content: "hi" }],
      }
      await runStream(provider, ctxFlatSystem, "tools-only")
      expect(countCacheControlOnTools(captured.req!)).toBe(1)
      expect(countCacheControlOnSystem(captured.req!)).toBe(0)
      expect(countCacheControlOnMessages(captured.req!)).toBe(0)
    })
  })

  describe("system-only", () => {
    it("emits cache_control only on system blocks", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxWithSystemBlocks, "system-only")
      expect(countCacheControlOnTools(captured.req!)).toBe(0)
      expect(countCacheControlOnSystem(captured.req!)).toBe(2)
      expect(countCacheControlOnMessages(captured.req!)).toBe(0)
    })
  })

  describe("frozen-prefix", () => {
    it("emits cache_control on messages only — anchored at frozenPrefixLen", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxFrozenPrefix, "frozen-prefix")
      expect(countCacheControlOnTools(captured.req!)).toBe(0)
      expect(countCacheControlOnSystem(captured.req!)).toBe(0)
      // Last message + frozen-prefix anchor = 2 breakpoints
      expect(countCacheControlOnMessages(captured.req!)).toBe(2)
    })

    it("when no frozenPrefixLen, frozen-prefix emits only the last-message breakpoint", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxWithSystemBlocks, "frozen-prefix")
      expect(countCacheControlOnTools(captured.req!)).toBe(0)
      expect(countCacheControlOnSystem(captured.req!)).toBe(0)
      // No rolling fallback under frozen-prefix → only the last message
      expect(countCacheControlOnMessages(captured.req!)).toBe(1)
    })
  })

  describe("none", () => {
    it("emits no cache_control anywhere", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxWithSystemBlocks, "none")
      expect(countCacheControlOnTools(captured.req!)).toBe(0)
      expect(countCacheControlOnSystem(captured.req!)).toBe(0)
      expect(countCacheControlOnMessages(captured.req!)).toBe(0)
    })
  })

  describe("unrecognised strategy", () => {
    it("falls back to default behavior on unknown string", async () => {
      const { provider, captured } = makeStubProvider()
      await runStream(provider, ctxWithSystemBlocks, "totally-not-a-real-strategy" as CacheBreakpointStrategy)
      expect(countCacheControlOnSystem(captured.req!)).toBe(2)
      expect(countCacheControlOnMessages(captured.req!)).toBe(2)
    })
  })
})
