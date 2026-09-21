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

test("context manager emits ledger events with stable fingerprints", () => {
  const events: Array<{ kind: string; itemId?: string; fingerprint: string }> = []
  const manager = new ContextManager({
    maxTokens: 100,
    now: () => 10,
    onEvent: event => events.push({ kind: event.kind, itemId: event.itemId, fingerprint: event.fingerprint }),
  })
  manager.upsert({ id: "b", kind: "memory", content: "second", scope: "turn", priority: 1, source: { type: "memory" } })
  manager.upsert({ id: "a", kind: "task", content: "first", scope: "turn", priority: 2, source: { type: "task" } })

  expect(events.map(event => event.kind)).toEqual(["context_added", "context_added"])
  expect(events[1]?.fingerprint).toBe(manager.fingerprint())
  expect(manager.snapshot().fingerprint).toBe(manager.fingerprint())

  manager.remove("a")
  expect(events.at(-1)?.kind).toBe("context_removed")
  expect(events.at(-1)?.itemId).toBe("a")
})

test("context manager emits expiration events without recursive ledger callbacks", () => {
  const events: string[] = []
  const manager = new ContextManager({
    maxTokens: 100,
    now: () => 20,
    onEvent: event => events.push(event.kind),
  })
  manager.upsert({ id: "leased", kind: "knowledge", content: "temporary", scope: "turn", priority: 1, expiresAt: 20, source: { type: "knowledge" } })

  expect(manager.expire()).toEqual(["leased"])
  expect(events).toEqual(["context_added", "context_expired"])
})

test("context manager keeps admission independent from a failing ledger sink", () => {
  const manager = new ContextManager({
    maxTokens: 20,
    onEvent: () => { throw new Error("telemetry offline") },
  })
  expect(() => manager.upsert({
    id: "stable",
    kind: "core",
    content: "policy",
    scope: "run",
    priority: 100,
    source: { type: "policy" },
  })).not.toThrow()
  expect(manager.snapshot().selected.map(item => item.id)).toEqual(["stable"])
})
