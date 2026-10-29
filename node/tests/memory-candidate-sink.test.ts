import { mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import {
  InMemoryMemoryCandidateSink,
  memoryCandidateId,
  memoryCandidatePayloadDigest,
  sessionExtractionCandidateId,
  type MemoryCandidate,
} from "../src/memory/candidate-sink.js"
import { FileMemoryCandidateSink } from "../src/memory/durable-candidate-sink.js"
import { Ed25519MemoryCandidateSourceVerifier, HmacMemoryCandidateSourceVerifier, memoryCandidateAttestationDigest, memoryCandidateAttestationCanonicalPayload, signMemoryCandidateAttestation } from "../src/memory/attestation.js"
import type { MemoryRecord } from "../src/memory/protocols.js"

/** M2 · the candidate sink contract (spec §2.3), pinned on the in-process implementation. */

const scope = { tenant_id: "t", namespace: "n" }

function record(content: string, overrides: Partial<MemoryRecord> = {}): MemoryRecord {
  return {
    record_id: "memory:any", scope, name: "brief-style", kind: "project", content, description: "",
    provenance: { author: "extraction", trust: "untrusted", evidence_refs: [] },
    created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.5, links: [], pinned: false,
    ...overrides,
  }
}

function candidate(id: string, content: string, overrides: Partial<MemoryRecord> = {}): MemoryCandidate {
  return { candidate_id: id, source: "session_extraction", record: record(content, overrides) }
}

describe("memory candidate sink (M2)", () => {
  it("holds a new candidate as pending with a receipt the sink minted", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const receipt = await sink.submit("agent", candidate("cand-1", "prefers numbered sections"))
    expect(receipt).toEqual({ status: "pending", candidate_id: "cand-1", receipt_id: expect.any(String) })
    expect(sink.candidates("agent")).toHaveLength(1)
  })

  it("answers a redelivered candidate with its original receipt and stores nothing new (S04)", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const first = await sink.submit("agent", candidate("cand-1", "prefers numbered sections"))
    // A retry may mint a fresh store id and timestamps; the proposal itself is unchanged.
    const again = await sink.submit("agent", candidate("cand-1", "prefers numbered sections", {
      record_id: "memory:other", created_at: 99, updated_at: 99,
    }))
    expect(again).toEqual(first)
    expect(sink.candidates()).toHaveLength(1)
  })

  it("rejects the same candidate id with a different payload and keeps what it holds", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const first = await sink.submit("agent", candidate("cand-1", "prefers numbered sections"))
    const conflict = await sink.submit("agent", candidate("cand-1", "prefers prose"))
    expect(conflict).toMatchObject({ status: "rejected", candidate_id: "cand-1", reason_code: "candidate_conflict" })
    expect(conflict.receipt_id).not.toBe(first.receipt_id)
    expect(sink.candidates().map(held => held.record.content)).toEqual(["prefers numbered sections"])
  })

  it("derives candidate ids from the write's identity, never at random", () => {
    expect(memoryCandidateId(["child_request", "op-1", "effect-3", "0"]))
      .toBe(memoryCandidateId(["child_request", "op-1", "effect-3", "0"]))
    expect(memoryCandidateId(["child_request", "op-1", "effect-3", "0"]))
      .not.toBe(memoryCandidateId(["child_request", "op-1", "effect-4", "0"]))
    // Part boundaries are part of the identity: ["a", "bc"] and ["ab", "c"] differ.
    expect(memoryCandidateId(["a", "bc"])).not.toBe(memoryCandidateId(["ab", "c"]))
  })

  it("a retried extraction that answers differently proposes new candidates instead of conflicting", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const propose = (content: string, overrides: Partial<MemoryRecord> = {}) => {
      const proposed = record(content, overrides)
      return { candidate_id: sessionExtractionCandidateId("agent", "s-1", "op-1", proposed), source: "session_extraction" as const, record: proposed }
    }
    const first = await sink.submit("agent", propose("prefers numbered sections"))
    // The retry re-asks the extraction model: the same proposal (fresh store id and timestamps)
    // repeats the id and gets the original receipt; a changed answer is a different candidate.
    const same = await sink.submit("agent", propose("prefers numbered sections", { record_id: "memory:retry", created_at: 9, updated_at: 9 }))
    const changed = await sink.submit("agent", propose("prefers numbered headings"))
    expect(same).toEqual(first)
    expect(changed).toMatchObject({ status: "pending" })
    expect(changed.candidate_id).not.toBe(first.candidate_id)
    expect(sink.candidates("agent")).toHaveLength(2)
  })

  it("scopes an extraction candidate id by agent, session and operation, and pins the cross-SDK vector", () => {
    const proposed = record("prefers numbered sections")
    const id = sessionExtractionCandidateId("agent", "s-1", "op-1", proposed)
    expect(id).toBe(memoryCandidateId(["v3", "agent", "session_extraction", "s-1", "op-1", memoryCandidatePayloadDigest(proposed)]))
    expect(sessionExtractionCandidateId("agent", "s-1", "op-2", proposed)).not.toBe(id)
    expect(sessionExtractionCandidateId("agent", "s-2", "op-1", proposed)).not.toBe(id)
    expect(sessionExtractionCandidateId("other", "s-1", "op-1", proposed)).not.toBe(id)
    expect(id).toBe(SESSION_EXTRACTION_VECTOR)
  })
})

// Same record, agent, session and operation in every SDK's candidate-sink test.
const SESSION_EXTRACTION_VECTOR = "candidate:8616f8c306bd0e0947ba3915403e02c1e47501fb8aed29e4d3a4ceec3e8207e7"

it("isolates an identical caller-supplied ID by agent", async () => {
  const sink = new InMemoryMemoryCandidateSink()
  const c = candidate("same-id", "proposal")
  const a = await sink.submit("a", c)
  const b = await sink.submit("b", c)
  expect(a.receipt_id).not.toBe(b.receipt_id)
  expect(sink.candidates("a")).toHaveLength(1)
  expect(sink.candidates("b")).toHaveLength(1)
  expect(await sink.submit("a", c)).toEqual(a)
})

it("verifies the shared Ed25519 attestation vector", async () => {
  const fixture = JSON.parse(await readFile(new URL("../../tests/fixtures/memory-attestation-ed25519.json", import.meta.url), "utf8"))
  const attestation = {
    issuer: "issuer-a", key_id: "ed-key-1", agent_id: "agent", tenant_id: "t", namespace: "n",
    candidate_id: "host-candidate", source: "session_extraction" as const, payload_digest: fixture.payload_digest,
    issued_at: 1_000, expires_at: 2_000, policy_version: "memory-attestation-v1", signature: fixture.signature,
  }
  expect(memoryCandidateAttestationCanonicalPayload(attestation)).toBe(fixture.canonical)
  expect(memoryCandidateAttestationDigest(attestation)).toBe(fixture.attestation_digest)
  const hostRecord = record("host fact", { provenance: { author: "host", trust: "host_verified", evidence_refs: [] } })
  const verifier = new Ed25519MemoryCandidateSourceVerifier({ trustedKeys: { "ed-key-1": { issuer: "issuer-a", publicKey: fixture.public_key } }, now: () => 1_500 })
  const result = verifier.verify("agent", { candidate_id: "host-candidate", source: "session_extraction", record: hostRecord, attestation })
  expect(result).toMatchObject({ valid: true })
})

it("requires and verifies host source attestation", async () => {
  const root = await mkdtemp(join(tmpdir(), "deepstrike-attestation-"))
  const path = join(root, "ledger.json")
  try {
    const hostRecord = record("host fact", { provenance: { author: "host", trust: "host_verified", evidence_refs: [] } })
    const hostCandidate = { ...candidate("host-candidate", "host fact"), record: hostRecord }
    const verifier = new HmacMemoryCandidateSourceVerifier({ trustedKeys: { "key-1": { issuer: "issuer-a", secret: "secret" } }, now: () => 1_500 })
    const sink = new FileMemoryCandidateSink({ path, sourceVerifier: verifier, now: () => 1_500 })
    const missing = await sink.submit("agent", hostCandidate)
    expect(missing).toMatchObject({ status: "rejected", reason_code: "attestation_missing" })
    const retryMissing = await sink.submit("agent", hostCandidate)
    expect(retryMissing).toMatchObject({ status: "rejected", reason_code: "attestation_missing" })
    expect(retryMissing.receipt_id).not.toBe(missing.receipt_id)
    expect(await sink.list("agent")).toEqual([])
    expect((await sink.verificationAttempts("agent"))[0]).toMatchObject({ reason_code: "attestation_missing" })

    const unsigned = {
      issuer: "issuer-a", key_id: "key-1", agent_id: "agent", tenant_id: "t", namespace: "n", candidate_id: hostCandidate.candidate_id,
      source: hostCandidate.source, payload_digest: memoryCandidatePayloadDigest(hostRecord), issued_at: 1_000, expires_at: 2_000,
      policy_version: "memory-attestation-v1",
    } as const
    const signed = { ...unsigned, signature: signMemoryCandidateAttestation(unsigned, "secret") }
    const accepted = await new FileMemoryCandidateSink({ path: join(root, "valid.json"), sourceVerifier: verifier }).submit("agent", { ...hostCandidate, attestation: signed })
    expect(accepted).toMatchObject({ status: "pending" })
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

it("persists candidates, terminal resolutions, and outbox across instances", async () => {
  const root = await mkdtemp(join(tmpdir(), "deepstrike-candidate-ledger-"))
  const path = join(root, "ledger.json")
  try {
    const first = new FileMemoryCandidateSink({ path, now: () => 100 })
    const pending = await first.submit("agent", candidate("durable", "proposal"))
    expect(pending.status).toBe("pending")
    const accepted = await first.resolve("agent", "durable", { status: "accepted", record_ref: "memory:durable", digest: "sha256:record" })
    expect(accepted).toMatchObject({ status: "accepted", receipt_id: pending.receipt_id })

    const recovered = new FileMemoryCandidateSink({ path, now: () => 200 })
    expect(await recovered.submit("agent", candidate("durable", "proposal"))).toEqual(accepted)
    expect(await recovered.resolve("agent", "durable", { status: "rejected", reason_code: "late" })).toEqual(accepted)
    expect((await recovered.list("agent"))[0].receipt).toEqual(accepted)
    expect((await recovered.outbox("agent")).map(event => event.type)).toEqual(["candidate_submitted", "candidate_accepted"])
    expect(await recovered.ackOutbox("outbox:1")).toBe(true)
    expect((await recovered.outbox("agent")).map(event => event.event_id)).toEqual(["outbox:2"])
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

it("fails closed when the durable candidate snapshot is corrupted", async () => {
  const root = await mkdtemp(join(tmpdir(), "deepstrike-candidate-corrupt-"))
  const path = join(root, "ledger.json")
  try {
    await (await import("node:fs/promises")).writeFile(path, "{broken", "utf8")
    await expect(new FileMemoryCandidateSink(path).list()).rejects.toThrow()
    expect(await readFile(path, "utf8")).toBe("{broken")
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

it("rejects changed source and review metadata without replacing the first proposal", async () => {
  const sink = new InMemoryMemoryCandidateSink()
  const c = candidate("c", "proposal")
  const original = await sink.submit("agent", c)
  const alternatives: MemoryCandidate[] = [
    { ...c, source: "child_request" },
    { ...c, record: { ...c.record, provenance: { ...c.record.provenance, trust: "host_verified" } } },
    { ...c, record: { ...c.record, provenance: { ...c.record.provenance, evidence_refs: ["new-evidence"] } } },
    { ...c, record: { ...c.record, confidence: 0.9 } },
    { ...c, record: { ...c.record, pinned: true } },
    { ...c, record: { ...c.record, links: ["related"] } },
    { ...c, record: { ...c.record, ttl_days: 7 } },
  ]
  for (const other of alternatives) {
    expect(await sink.submit("agent", other)).toMatchObject({ status: "rejected", reason_code: "candidate_conflict" })
  }
  expect(await sink.submit("agent", c)).toEqual(original)
  expect(sink.candidates()).toEqual([c])
})

it("snapshots input, output and receipt so callers cannot mutate held state", async () => {
  const sink = new InMemoryMemoryCandidateSink()
  const c = candidate("c", "proposal")
  const receipt = await sink.submit("agent", c)
  const original = { ...receipt }
  c.record.content = "tampered"
  c.record.provenance.evidence_refs.push("tampered")
  receipt.receipt_id = "tampered"
  sink.candidates()[0].record.links.push("tampered")
  expect(await sink.submit("agent", candidate("c", "proposal"))).toEqual(original)
  expect(sink.candidates()[0].record.content).toBe("proposal")
  expect(sink.candidates()[0].record.links).toEqual([])
})
