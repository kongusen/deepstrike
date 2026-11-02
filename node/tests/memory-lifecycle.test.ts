import { mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { consumeMemoryLeaseWrite, memoryRecordExpired, revokeMemoryLease, verifyMemoryLease } from "../src/memory/lifecycle.js"
import { FileMemoryLifecycleStore } from "../src/memory/durable-lifecycle-store.js"
import type { MemoryLease } from "../src/memory/lifecycle.js"

test("memory lease enforces scope, expiry, revocation, and write budget", () => {
  const lease: MemoryLease = { lease_id: "l", agent_id: "a", scope: { tenant_id: "t", namespace: "n" }, issued_at: 0, expires_at: 100, max_writes: 1, writes_used: 0 }
  expect(verifyMemoryLease(lease, "a", lease.scope, 50)).toEqual({ valid: true })
  expect(verifyMemoryLease(lease, "b", lease.scope, 50)).toMatchObject({ reason_code: "lease_scope_mismatch" })
  expect(verifyMemoryLease(lease, "a", lease.scope, 100)).toMatchObject({ reason_code: "lease_expired" })
  expect(verifyMemoryLease(revokeMemoryLease(lease, 40), "a", lease.scope, 50)).toMatchObject({ reason_code: "lease_revoked" })
  expect(verifyMemoryLease(consumeMemoryLeaseWrite(lease), "a", lease.scope, 50)).toMatchObject({ reason_code: "memory_write_budget_exhausted" })
})

test("TTL expiration uses updated_at and day precision", () => {
  const record = { updated_at: 1_000, ttl_days: 1 }
  expect(memoryRecordExpired(record, 1_000 + 86_400_000)).toBe(false)
  expect(memoryRecordExpired(record, 1_000 + 86_400_001)).toBe(true)
})

test("file lifecycle store atomically consumes a lease and survives restart", async () => {
  const directory = await mkdtemp(join(tmpdir(), "deepstrike-memory-lifecycle-"))
  const path = join(directory, "lifecycle.json")
  const lease: MemoryLease = {
    lease_id: "lease-1", agent_id: "agent-1", scope: { tenant_id: "tenant-1", namespace: "ns-1" },
    issued_at: 10, expires_at: 1_000, max_writes: 1, writes_used: 0,
  }
  try {
    const first = new FileMemoryLifecycleStore({ path, now: () => 100 })
    await first.issueLease(lease)
    expect((await first.consumeWrite("agent-1", lease.scope, "op-1", 100)).valid).toBe(true)
    expect((await first.consumeWrite("agent-1", lease.scope, "op-1", 100)).valid).toBe(true)

    const restarted = new FileMemoryLifecycleStore({ path, now: () => 100 })
    expect((await restarted.getLease("agent-1", lease.scope))?.writes_used).toBe(1)
    expect((await restarted.consumeWrite("agent-1", lease.scope, "op-2", 100))).toMatchObject({
      valid: false, reason_code: "memory_write_budget_exhausted",
    })
    await restarted.renewLease("agent-1", "lease-1", 2_000, 200)
    await restarted.revokeLease("agent-1", "lease-1", 300)
    const events = await restarted.events()
    expect(events.map(event => event.type)).toEqual([
      "lease_issued", "lease_write_consumed", "lease_renewed", "lease_revoked",
    ])
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test("file lifecycle deletion is request-then-apply and retries after a crash window", async () => {
  const directory = await mkdtemp(join(tmpdir(), "deepstrike-memory-lifecycle-"))
  const path = join(directory, "lifecycle.json")
  const scope = { tenant_id: "tenant-1", namespace: "ns-1" }
  let attempts = 0
  try {
    const store = new FileMemoryLifecycleStore({
      path,
      now: () => 100,
      deletePhysical: async () => {
        attempts += 1
        if (attempts === 1) throw new Error("simulated crash")
        return true
      },
    })
    await expect(store.deleteRecord("agent-1", scope, "record-1", "delete-1")).rejects.toThrow("simulated crash")
    expect((await store.events()).map(event => event.type)).toEqual(["deletion_requested"])
    const receipt = await store.deleteRecord("agent-1", scope, "record-1", "delete-1")
    expect(receipt).toMatchObject({ deletion_id: "delete-1", status: "applied", event_sequence: 1 })
    expect((await store.events()).map(event => event.type)).toEqual(["deletion_requested", "deletion_applied"])
    expect(await store.deleteRecord("agent-1", scope, "record-1", "delete-1")).toEqual(receipt)
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test("lifecycle replay rejects conflicting duplicate event ids", async () => {
  const { replayMemoryLifecycle } = await import("../src/memory/lifecycle.js")
  const event = {
    event_id: "lease_issued:lease-1:0", sequence: 0, type: "lease_issued" as const,
    agent_id: "agent-1", lease_id: "lease-1", scope: { tenant_id: "tenant-1", namespace: "ns-1" },
    occurred_at: 10, payload: { max_writes: 1, expires_at: 100 },
  }
  expect(() => replayMemoryLifecycle([event, { ...event, payload: { max_writes: 2, expires_at: 100 } }])).toThrow("event conflict")
  expect(() => replayMemoryLifecycle([event, { ...event, sequence: 2, event_id: "lease_renewed:lease-1:1", type: "lease_renewed", payload: { expires_at: 200 } }])).toThrow("sequence gap")
})

test("file lifecycle store rejects corrupt snapshots", async () => {
  const directory = await mkdtemp(join(tmpdir(), "deepstrike-memory-lifecycle-"))
  const path = join(directory, "lifecycle.json")
  try {
    await import("node:fs/promises").then(fs => fs.writeFile(path, "{}"))
    const store = new FileMemoryLifecycleStore(path)
    await expect(store.events()).rejects.toThrow("invalid memory lifecycle snapshot")
    await expect(readFile(path, "utf8")).resolves.toBe("{}")
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})
