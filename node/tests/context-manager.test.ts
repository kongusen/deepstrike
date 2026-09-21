import { ContextManager } from "../src/runtime/context-manager.js"

test("context manager selects pinned and higher priority items within a response reserve", () => {
  const manager = new ContextManager({ maxTokens: 20, responseReserveTokens: 5, now: () => 100 })
  manager.upsert({ id: "low", kind: "memory", content: "x".repeat(20), scope: "turn", priority: 1, source: { type: "memory" } })
  manager.upsert({ id: "high", kind: "task", content: "x".repeat(20), scope: "step", priority: 10, source: { type: "task" } })
  manager.upsert({ id: "pinned", kind: "core", content: "x".repeat(40), scope: "run", priority: 0, pinned: true, source: { type: "policy" } })
  expect(manager.snapshot().selected.map(item => item.id)).toEqual(["pinned", "high"])
  expect(manager.snapshot().availableTokens).toBe(0)
})

test("context manager expires non-pinned leased items", () => {
  const manager = new ContextManager({ maxTokens: 100, now: () => 10 })
  manager.upsert({ id: "leased", kind: "skill", content: "skill", scope: "turn", priority: 1, expiresAt: 10, source: { type: "skill", id: "debug" } })
  manager.upsert({ id: "pinned", kind: "core", content: "policy", scope: "run", priority: 1, expiresAt: 10, pinned: true, source: { type: "policy" } })
  expect(manager.expire()).toEqual(["leased"])
  expect(manager.snapshot().items.map(item => item.id)).toEqual(["pinned"])
})
