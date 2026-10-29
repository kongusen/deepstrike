import { consumeMemoryLeaseWrite, memoryRecordExpired, revokeMemoryLease, verifyMemoryLease } from "../src/memory/lifecycle.js"
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
