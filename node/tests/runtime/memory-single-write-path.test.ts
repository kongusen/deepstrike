import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { collectText } from "../../src/runtime/runner.js"
import { createRunner } from "./helpers.js"
import type { LLMProvider, ModelMessage, StreamEvent } from "../../src/types.js"
import type { MemoryStore, MemoryRecord } from "../../src/memory/protocols.js"

describe("M2 memory single-write path", () => {
  it("saves the transcript without performing automatic extraction", async () => {
    const order: string[] = []
    const persisted: MemoryRecord[] = []
    let providerCalls = 0
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        providerCalls += 1
        yield { type: "text_delta", delta: "Use focused tests." }
      },
    }
    const memoryStore = {
      search: async () => [],
      saveSession: async () => { order.push("save") },
      get: async () => null,
      delete: async () => {},
      put: async (_agentId: string, record: MemoryRecord) => {
        order.push("put")
        persisted.push(record)
      },
    } satisfies MemoryStore
    const { runner, sessionLog } = createRunner(provider, [], {
      agentId: "agent-m2",
      memoryScope: { tenant_id: "tenant-m2", namespace: "assistant" },
      memoryStore,
    })

    await collectText(runner.run({ sessionId: "session-m2", goal: "Remember how I test" }))

    expect(providerCalls).toBe(1)
    expect(order).toEqual(["save"])
    expect(persisted).toHaveLength(0)
    const events = await sessionLog.read("session-m2")
    expect(events.filter(entry => entry.event.kind === "memory_written")).toHaveLength(0)
  })

  it("contains no alternate durable-memory write path", () => {
    const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..")
    const sources = [
      "src/runtime/runner.ts",
      "src/memory/protocols.ts",
      "../crates/deepstrike-core/src/memory/mod.rs",
    ].map(relative => fs.readFileSync(path.resolve(root, relative), "utf8")).join("\n")

    expect(sources).not.toMatch(/legacy_memory_batch|memoryStore\.commit|store\.commit/)
  })
})
