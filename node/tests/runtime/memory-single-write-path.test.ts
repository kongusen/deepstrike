import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { collectText } from "../../src/runtime/runner.js"
import { createRunner } from "./helpers.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"
import type { DreamStore, MemoryRecord } from "../../src/memory/protocols.js"

describe("M2 memory single-write path", () => {
  it("extracts once after saveSession and journals every extracted record through WriteMemory", async () => {
    const order: string[] = []
    const persisted: MemoryRecord[] = []
    let providerCalls = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        providerCalls += 1
        if (providerCalls === 1) {
          yield { type: "text_delta", delta: "Use focused tests." }
          return
        }
        yield {
          type: "text_delta",
          delta: JSON.stringify({
            memories: [{
              name: "prefers-focused-tests",
              kind: "feedback",
              content: "Use focused tests.",
              description: "Stable testing preference",
              confidence: 0.9,
              evidence_refs: ["assistant:final"],
            }],
          }),
        }
      },
    }
    const dreamStore = {
      search: async () => [],
      saveSession: async () => { order.push("save") },
      upsert: async (_agentId: string, record: MemoryRecord) => {
        order.push("upsert")
        persisted.push(record)
      },
    } as DreamStore & { upsert(agentId: string, record: MemoryRecord): Promise<void> }
    const { runner, sessionLog } = createRunner(provider, [], {
      agentId: "agent-m2",
      memoryScope: { tenant_id: "tenant-m2", namespace: "assistant" },
      dreamStore,
    })

    await collectText(runner.run({ sessionId: "session-m2", goal: "Remember how I test" }))

    expect(providerCalls).toBe(2)
    expect(order).toEqual(["save", "upsert"])
    expect(persisted).toHaveLength(1)
    expect(persisted[0]).toMatchObject({
      scope: { tenant_id: "tenant-m2", namespace: "assistant" },
      name: "prefers-focused-tests",
      kind: "feedback",
      content: "Use focused tests.",
      provenance: { session_id: "session-m2", author: "extraction", trust: "untrusted" },
    })
    const events = await sessionLog.read("session-m2")
    expect(events.filter(entry => entry.event.kind === "memory_written")).toHaveLength(1)
  })

  it("contains no idle state machine or alternate DreamStore commit path", () => {
    const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..")
    const sources = [
      "src/runtime/runner.ts",
      "src/memory/protocols.ts",
      "../crates/deepstrike-core/src/memory/mod.rs",
    ].map(relative => fs.readFileSync(path.resolve(root, relative), "utf8")).join("\n")

    expect(sources).not.toMatch(/IdlePipeline|idle_pipeline|CommitMemories|loadSessions|load_memories/)
    expect(sources).not.toMatch(/dreamStore\.commit|store\.commit/)
  })
})
