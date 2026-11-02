/**
 * Strict dynamic context control after Task 21 canonical cutover.
 *
 * legacy kinds (`add_knowledge_message`, `skill_activated`, `configure_run`, `add_history_message`)
 * are lowered into canonical inputs. These tests assert the durable canonical shapes the mock
 * kernel receives via `kernelEvents`.
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { MemoryStore, MemoryRecall } from "../src/memory/index.js"
import type { LLMProvider, ModelMessage, StreamEvent } from "../src/types.js"
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
      async complete(): Promise<ModelMessage> {
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
    let calls = 0
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        calls += 1
        // `skill` is a kernel syscall, so there is no host tool_result to react to: deactivate on
        // the turn after the activation instead.
        if (calls === 1) {
          yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
          return
        }
        await runnerRef?.deactivateSkill("debug")
        yield { type: "text_delta", delta: "done" }
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

    for await (const _e of runner.run({ sessionId: "skill-lease", goal: "debug it" })) { /* drain */ }

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
      async complete(): Promise<ModelMessage> {
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

describe("memory recall is the kernel's", () => {
  it("a scoped memory store turns on kernel recall; the host seeds no recall itself", async () => {
    kernelEvents.length = 0
    const scope = { tenant_id: "agent-prequery", namespace: "prefetch" }
    const memoryStore: MemoryStore = {
      put: async () => {},
      get: async () => null,
      delete: async () => {},
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
      async complete(): Promise<ModelMessage> {
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
      memoryStore,
      preQueryMemory: () => [{ scope, query: "past facts", top_k: 5, kinds: [] }],
    })

    for await (const _e of runner.run({ sessionId: "prequery", goal: "use the fact" })) { /* drain */ }

    const configure = kernelEvents.find((e: { kind?: string }) => e.kind === "configure_operation") as
      | { config?: { memory_policy?: { recall?: boolean }; memory_access?: { capabilities?: { read?: boolean } } } }
      | undefined
    expect(configure?.config?.memory_policy?.recall).toBe(true)
    expect(configure?.config?.memory_access?.capabilities?.read).toBe(true)
    const start = kernelEvents.find((e: { kind?: string }) => e.kind === "start_operation") as
      | { initial_context?: { knowledge?: Array<{ content?: string }> } }
      | undefined
    expect((start?.initial_context?.knowledge ?? []).some(k => String(k.content).startsWith("[MEMORY "))).toBe(false)
  })
})

describe("dependency outputs reach the child kernel", () => {
  it("lands dependencyOutputs in start_operation initial_context.dependencies", async () => {
    kernelEvents.length = 0
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
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
      dependencyOutputs: { "wf-node0": "FULL_SURVEY_BODY" },
    })
    for await (const _e of runner.run({ sessionId: "deps", goal: "synthesize" })) { /* drain */ }
    const start = kernelEvents.find((e: { kind?: string }) => e.kind === "start_operation") as
      | { initial_context?: { dependencies?: Array<{ agent_id: string; content: string }> } }
      | undefined
    expect(start?.initial_context?.dependencies).toEqual([{ agent_id: "wf-node0", content: "FULL_SURVEY_BODY" }])
  })
})
