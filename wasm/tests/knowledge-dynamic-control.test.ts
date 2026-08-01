/**
 * Strict dynamic context control after Task 21 canonical cutover.
 *
 * legacy kinds (`add_knowledge_message`, `skill_activated`, `configure_run`, `add_history_message`)
 * are lowered into canonical inputs. These tests assert the durable canonical shapes the mock
 * kernel receives via `kernelEvents`.
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { DreamStore, MemoryRecall } from "../src/memory/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"
import { kernelEvents } from "@deepstrike/wasm-kernel"

function hostControls() {
  return kernelEvents.filter((event: { kind?: string }) => event.kind === "host_control") as Array<{
    kind: string
    command?: Record<string, unknown>
  }>
}

describe("skill content is pinned into durable knowledge on activation", () => {
  it("emits seed_knowledge with the skill's resolved content on activation", async () => {
    kernelEvents.length = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
      },
    }

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 6,
      skillContentMap: new Map([["debug", "---\nname: debug\n---\nDebug guidance."]]),
    })

    for await (const _e of runner.run({ sessionId: "knowledge-pin", goal: "debug it" })) { /* drain */ }

    const knowledgePushes = hostControls().filter(event => event.command?.kind === "seed_knowledge")
    expect(knowledgePushes.length).toBeGreaterThanOrEqual(1)
    const entries = (knowledgePushes[0]!.command as { entries?: Array<Record<string, unknown>> }).entries ?? []
    expect(JSON.stringify(entries)).toContain("Debug guidance.")
    expect(entries.some(entry => entry.key === "skill:debug")).toBe(true)
  })
})

describe("skill lease + deactivation events reach the kernel (K3)", () => {
  it("deactivateSkill emits apply_skill_activation deactivate under host_control", async () => {
    kernelEvents.length = 0
    let runnerRef: RuntimeRunner | undefined
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
      },
    }

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 6,
      skillContentMap: new Map([["debug", "---\nname: debug\n---\nDebug guidance."]]),
      skillLeaseTurns: 3,
    })
    runnerRef = runner

    for await (const e of runner.run({ sessionId: "skill-lease", goal: "debug it" })) {
      if (e.type === "tool_result" && runnerRef) runnerRef.deactivateSkill("debug")
    }

    const deactivated = hostControls().some(event =>
      event.command?.kind === "apply_skill_activation"
      && Array.isArray((event.command as { deactivate?: string[] }).deactivate)
      && (event.command as { deactivate: string[] }).deactivate.includes("debug"))
    expect(deactivated).toBe(true)
  })
})

describe("knowledgeBudgetRatio reaches the kernel via configure_operation (K2)", () => {
  it("carries knowledge_budget_ratio in the configure_operation bundle", async () => {
    kernelEvents.length = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "done" }
      },
    }

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      knowledgeBudgetRatio: 0.1,
    })

    for await (const _e of runner.run({ sessionId: "budget-knob", goal: "noop" })) { /* drain */ }

    const configure = kernelEvents.find((e: { kind?: string }) => e.kind === "configure_operation") as
      | { config?: { context_policy?: { knowledge_budget_ppm?: number } } }
      | undefined
    // Canonical lowering stores the ratio as ppm on context_policy.
    expect(configure?.config?.context_policy?.knowledge_budget_ppm).toBe(100_000)
  })
})

describe("preQueryMemory prefetch lands in initial history, not knowledge", () => {
  it("seeds start_operation history with prefetch content, never seed_knowledge/page_in", async () => {
    kernelEvents.length = 0
    const scope = { tenant_id: "agent-prequery", namespace: "prefetch" }
    const dreamStore: DreamStore = {
      loadSessions: async () => [],
      loadMemories: async () => [],
      commit: async () => {},
      saveSession: async () => {},
      search: async () => [{
        record: {
          record_id: "record-prefetch", scope, name: "prefetch", kind: "reference",
          content: "PREFETCHED_LONGTERM_FACT", description: "fixture",
          provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
          created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
        }, score: 0.9, why: "fixture",
      } satisfies MemoryRecall],
    }

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "done" }
      },
    }

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      agentId: "agent-prequery",
      memoryScope: scope,
      dreamStore,
      preQueryMemory: () => [{ scope, query: "past facts", top_k: 5, kinds: [] }],
    })

    for await (const _e of runner.run({ sessionId: "prequery", goal: "use the fact" })) { /* drain */ }

    const start = kernelEvents.find((e: { kind?: string }) => e.kind === "start_operation") as
      | { initial_context?: { messages?: unknown[] } }
      | undefined
    expect(JSON.stringify(start?.initial_context?.messages ?? [])).toContain("PREFETCHED_LONGTERM_FACT")
    expect(hostControls().some(event => event.command?.kind === "seed_knowledge")).toBe(false)
    expect(kernelEvents.some((e: { kind?: string }) => e.kind === "page_in")).toBe(false)
  })
})
