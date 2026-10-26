import { getKernel } from "../../src/kernel.js"
import { InMemoryKernelJournal } from "../../src/runtime/kernel-journal.js"
import { CanonicalRunnerRuntime } from "../../src/runtime/canonical-kernel-step.js"
import { RuntimeRunner } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import type { LLMProvider, ModelMessage, StreamEvent } from "../../src/types.js"
import type { MemoryStore } from "../../src/memory/protocols.js"

/** The kernel binding is the enforcement point for memory capabilities: plan_memory_write and
 *  plan_memory_query refuse a syscall whose bit is off. These tests pin the projection from the
 *  host's set_memory_enabled event into the kernel config, and the runner's wiring of the bits. */

function bootstrapRuntime(): CanonicalRunnerRuntime {
  const { CanonicalKernel } = getKernel()
  return new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-memory-binding", {
    maxContextTokens: 100_000,
  })
}

function memoryAccessOf(runtime: CanonicalRunnerRuntime): unknown {
  return (runtime as unknown as { config: { memory_access?: unknown } }).config.memory_access
}

function applyMemoryEnabled(runtime: CanonicalRunnerRuntime, event: Record<string, unknown>): void {
  ;(runtime as unknown as { applyBootstrapEvent: (e: Record<string, unknown>) => boolean }).applyBootstrapEvent(event)
}

describe("memory kernel binding carries the host capability bits", () => {
  it("grants read and write when the bits are omitted (existing behavior)", () => {
    const runtime = bootstrapRuntime()
    applyMemoryEnabled(runtime, { kind: "set_memory_enabled", enabled: true })
    expect(memoryAccessOf(runtime)).toEqual({ binding_id: "node-memory", capabilities: { read: true, write: true } })
  })

  it("projects a write-off binding (modelWrites disabled)", () => {
    const runtime = bootstrapRuntime()
    applyMemoryEnabled(runtime, { kind: "set_memory_enabled", enabled: true, read: true, write: false })
    expect(memoryAccessOf(runtime)).toEqual({ binding_id: "node-memory", capabilities: { read: true, write: false } })
  })

  it("projects a read-off binding (retrieval disabled)", () => {
    const runtime = bootstrapRuntime()
    applyMemoryEnabled(runtime, { kind: "set_memory_enabled", enabled: true, read: false, write: true })
    expect(memoryAccessOf(runtime)).toEqual({ binding_id: "node-memory", capabilities: { read: false, write: true } })
  })

  it("leaves no binding when memory is not enabled", () => {
    const runtime = bootstrapRuntime()
    applyMemoryEnabled(runtime, { kind: "set_memory_enabled", enabled: false, read: false, write: false })
    expect(memoryAccessOf(runtime)).toBeUndefined()
  })
})

describe("runner passes the policy bits into the kernel binding", () => {
  const provider: LLMProvider = {
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(): AsyncIterable<StreamEvent> {
      yield { type: "text_delta", delta: "done" }
    },
  }
  const store: MemoryStore = {
    put: async () => {},
    get: async () => null,
    delete: async () => {},
    saveSession: async () => {},
    search: async () => [],
  }

  /** Runs the runner once and returns the set_memory_enabled event it committed to the kernel. */
  async function memoryEnabledEvent(policy: Record<string, unknown> | undefined): Promise<Record<string, unknown> | undefined> {
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-binding",
      memoryScope: { tenant_id: "agent-binding", namespace: "binding" },
      memoryStore: store,
      ...(policy ? { memoryRuntimePolicy: policy as never } : {}),
    })
    const committed: unknown[][] = []
    const host = runner as unknown as { commitKernelApply: (...args: unknown[]) => unknown }
    const original = host.commitKernelApply.bind(runner)
    host.commitKernelApply = (...args: unknown[]) => {
      committed.push(args)
      return original(...args)
    }
    try {
      for await (const _event of runner.run({ sessionId: "binding", goal: "say done" })) {
        // drain
      }
    } catch {
      // The run's outcome is not under test; only the committed bootstrap event is.
    }
    const call = committed.find(args => (args[2] as { kind?: string })?.kind === "set_memory_enabled")
    return call?.[2] as Record<string, unknown> | undefined
  }

  it("sends read and write from the resolved policy", async () => {
    const event = await memoryEnabledEvent({
      retrieval: "host",
      sessionExtraction: "disabled",
      modelWrites: "disabled",
      semanticArchive: "disabled",
    })
    expect(event).toMatchObject({ kind: "set_memory_enabled", enabled: true, read: true, write: false })
  })

  it("turns read off when retrieval is disabled", async () => {
    const event = await memoryEnabledEvent({
      retrieval: "disabled",
      sessionExtraction: "disabled",
      modelWrites: "disabled",
      semanticArchive: "disabled",
    })
    expect(event).toMatchObject({ kind: "set_memory_enabled", read: false, write: false })
  })

  it("sends both bits on when no policy is set", async () => {
    const event = await memoryEnabledEvent(undefined)
    expect(event).toMatchObject({ kind: "set_memory_enabled", enabled: true, read: true, write: true })
  })
})
