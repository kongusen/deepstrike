import { createHash } from "node:crypto"

import type { MemoryCandidateReceipt } from "../src/memory/candidate-sink.js"
import { InMemoryKernelJournal, type KernelJournal } from "../src/runtime/kernel-journal.js"
import { InMemorySessionLog, type SessionEvent } from "../src/runtime/session-log.js"
import {
  SessionEvidenceReader,
  SharedEvidenceBuilder,
  createSharedEvidence,
  knowledgeIdentity,
  readSessionEvidence,
  type EvidenceRef,
} from "../src/runtime/shared-evidence.js"

const scope = {
  operationId: "op-1",
  sessionId: "session-1",
  agentId: "agent-1",
  tenantId: "tenant-1",
  namespace: "default",
} as const
const inputScope = {
  sessionId: scope.sessionId,
  agentId: scope.agentId,
  tenantId: scope.tenantId,
  namespace: scope.namespace,
}
const journal = [new Uint8Array([1])]

function ref(overrides: Partial<EvidenceRef> = {}): EvidenceRef {
  return {
    refId: "ref-1",
    domain: "provider_attempt",
    authority: "host_observation",
    scope,
    contentDigest: "sha256:attempt",
    availability: "resident",
    ...overrides,
  }
}

describe("shared evidence read model", () => {
  it("copies verification bytes per build and creates immutable facets", () => {
    const bytes = new Uint8Array([1, 2])
    const builder = new SharedEvidenceBuilder({
      operationId: "op-1",
      scope: inputScope,
      asOf: { journalStepSeq: 42 },
      verification: { journal: [bytes], sessionLogs: [[new Uint8Array([3])]], checkpoints: [new Uint8Array([4])] },
      refs: [
        ref({ refId: "memory", domain: "memory_record", authority: "derived_projection" }),
        ref(),
        ref({ refId: "context", domain: "context_binding" }),
      ],
    })
    const snapshot = builder.build()

    bytes[0] = 9
    expect([...snapshot.verification.journal[0]]).toEqual([1, 2])
    snapshot.verification.journal[0][0] = 7
    snapshot.verification.checkpoints![0][0] = 7
    expect([...snapshot.verification.journal[0]]).toEqual([1, 2])
    expect([...snapshot.verification.checkpoints![0]]).toEqual([4])
    expect([...builder.build().verification.journal[0]]).toEqual([1, 2])
    expect(snapshot.asOf).toEqual({ journalStepSeq: 42 })
    expect(Object.isFrozen(snapshot.asOf)).toBe(true)
    expect(snapshot.facets.execution?.map(item => item.refId)).toEqual(["context", "ref-1"])
    expect(snapshot.facets.references?.map(item => item.refId)).toEqual(["memory"])
    expect(Object.isFrozen(snapshot)).toBe(true)
    expect(Object.isFrozen(snapshot.refs)).toBe(true)
    expect(Object.isFrozen(snapshot.refs[0])).toBe(true)
  })

  it("orders refs by ordinal (domain, refId) independent of locale", () => {
    const snapshot = createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [
        ref({ refId: "b" }),
        ref({ refId: "a" }),
        ref({ refId: "B" }),
        ref({ refId: "z", domain: "checkpoint", authority: "kernel_fact" }),
      ],
    })
    // localeCompare would place "a" before "B"; ordinal places uppercase first.
    expect(snapshot.refs.map(item => `${item.domain}/${item.refId}`))
      .toEqual(["checkpoint/z", "provider_attempt/B", "provider_attempt/a", "provider_attempt/b"])
  })

  it("rejects mixed scopes and conflicting refs within one domain", () => {
    expect(() => createSharedEvidence({
      operationId: "op-1",
      verification: { journal },
      refs: [ref({ scope: { ...scope, operationId: "other-op" } })],
    })).toThrow("crosses the shared evidence scope")

    expect(() => createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref(), ref({ contentDigest: "sha256:other" })],
    })).toThrow("conflicting evidence ref: provider_attempt/ref-1")
  })

  it("treats equal refIds in different domains as distinct evidence", () => {
    const snapshot = createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref(), ref({ domain: "session_event", contentDigest: "sha256:event" })],
    })
    const reader = new SessionEvidenceReader(snapshot)
    expect(snapshot.refs).toHaveLength(2)
    expect(reader.byRefId("provider_attempt", "ref-1")?.contentDigest).toBe("sha256:attempt")
    expect(reader.byRefId("session_event", "ref-1")?.contentDigest).toBe("sha256:event")
    expect(reader.byRefId("checkpoint", "ref-1")).toBeUndefined()
    expect(snapshot.diagnostics).toHaveLength(0)
  })

  it("merges identical duplicates with a diagnostic", () => {
    const snapshot = createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref(), ref()],
    })
    expect(snapshot.refs).toHaveLength(1)
    expect(snapshot.completeness).toBe("complete")
    expect(snapshot.diagnostics).toEqual([expect.objectContaining({
      code: "duplicate_evidence_ref", domain: "provider_attempt", refId: "ref-1",
    })])
  })

  it("supports read-only domain and digest queries", () => {
    const reader = new SessionEvidenceReader(createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref(), ref({ refId: "context", domain: "context_binding", contentDigest: "sha256:context" })],
    }))
    expect(reader.refs("context_binding").map(item => item.refId)).toEqual(["context"])
    expect(reader.byDigest("sha256:context").map(item => item.refId)).toEqual(["context"])
  })

  it("derives partial completeness from refs regardless of the declared value", () => {
    const snapshot = createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      completeness: "complete",
      refs: [
        ref({ refId: "kb", domain: "knowledge_entry", authority: "derived_projection", contentDigest: undefined }),
        ref({ refId: "gone", availability: "missing" }),
      ],
    })
    expect(snapshot.completeness).toBe("partial")
    expect(snapshot.diagnostics.map(item => `${item.code}:${item.refId}`))
      .toEqual(["knowledge_digest_missing:kb", "knowledge_provenance_missing:kb", "evidence_missing:gone"])
  })

  it("requires provenance as well as a digest for complete knowledge", () => {
    const knowledge = (overrides: Partial<EvidenceRef>) => createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref({ refId: "kb", domain: "knowledge_entry", authority: "derived_projection", ...overrides })],
    })
    expect(knowledge({}).diagnostics.map(item => item.code)).toEqual(["knowledge_provenance_missing"])
    expect(knowledge({ provenance: "kb:handbook" }).completeness).toBe("complete")
  })

  it("keeps retention as an observation and settlement on memory refs only", () => {
    const snapshot = createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref({
        refId: "m", domain: "memory_record", settlement: "pending", retention: { observedTtlDays: 30, pinned: true },
      })],
    })
    expect(snapshot.refs[0]).toMatchObject({ settlement: "pending", retention: { observedTtlDays: 30, pinned: true } })
    expect(() => createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref({ settlement: "accepted" })],
    })).toThrow("settlement is only valid on memory refs")
  })

  it("derives unavailable for an empty snapshot and keeps declared diagnostics", () => {
    const snapshot = createSharedEvidence({
      operationId: "op-1",
      verification: { journal: [] },
      diagnostics: [{ code: "knowledge_reference_unavailable", message: "page-in contained no stable references" }],
    })
    expect(snapshot.completeness).toBe("unavailable")
    expect(snapshot.refs).toHaveLength(0)
    expect(snapshot.diagnostics.map(item => item.code))
      .toEqual(["knowledge_reference_unavailable", "evidence_empty"])
  })

  it("validates cursor positions and evidence ref ids", () => {
    const build = (input: Partial<Parameters<typeof createSharedEvidence>[0]>) => createSharedEvidence({
      operationId: "op-1", scope: inputScope, verification: { journal }, ...input,
    })
    expect(build({ asOf: { sessionLogSeq: -1, checkpointOrdinal: 0 } }).asOf).toEqual({ sessionLogSeq: -1, checkpointOrdinal: 0 })
    expect(() => build({ asOf: { sessionLogSeq: -2 } })).toThrow("asOf.sessionLogSeq")
    expect(() => build({ asOf: { checkpointOrdinal: 1.5 } })).toThrow("asOf.checkpointOrdinal")
    expect(() => build({ refs: [ref({ evidenceRefs: [""] })] })).toThrow("evidenceRefs[]")
    const source = ["turn:1"]
    const snapshot = build({ refs: [ref({ evidenceRefs: source })] })
    source.push("turn:2")
    expect(snapshot.refs[0]?.evidenceRefs).toEqual(["turn:1"])
    expect(Object.isFrozen(snapshot.refs[0]?.evidenceRefs)).toBe(true)
  })

  it("never upgrades a declared partial snapshot to complete", () => {
    const snapshot = new SharedEvidenceBuilder({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref()],
    }).markPartial({ code: "sidecar_unreadable", message: "sidecar read failed" }).build()
    expect(snapshot.completeness).toBe("partial")
  })
})

describe("readSessionEvidence", () => {
  const memoryScope = { tenant_id: "tenant-1", namespace: "default" }
  const sourceScope = { agentId: "agent-1", tenantId: "tenant-1", namespace: "default" }
  const contextPrepared = {
    kind: "context_prepared",
    turn: 1,
    effect_id: "effect-1",
    preparation: {
      binding: { digest: "sha256:binding" },
      state: { knowledge: [{ entry_id: "kb-1", content_digest: "sha256:kb", source: "knowledge", ordinal: 0 }] },
    },
  } as unknown as SessionEvent
  const attempt = {
    kind: "provider_attempt",
    effect_id: "effect-1",
    attempt_seq: 1,
    request_fingerprint: "fp-1",
    status: "success",
    transport_rungs: 1,
    started_at_ms: 1,
    finished_at_ms: 2,
  } as unknown as SessionEvent

  const recalled = {
    record_id: "mem-1",
    scope: memoryScope,
    pinned: false,
    ttl_days: 7,
    provenance: { author: "agent", trust: "observed", evidence_refs: ["turn:1", "tool:t-1"] },
  }

  async function seed(): Promise<{ sessionLog: InMemorySessionLog, kernelJournal: KernelJournal }> {
    const sessionLog = new InMemorySessionLog()
    for (const event of [
      contextPrepared,
      attempt,
      { kind: "memory_written", turn: 1, record_id: "mem-1", scope: memoryScope, memory_kind: "fact", name: "n", size_bytes: 12 },
      { kind: "memory_written", turn: 1, record_id: "mem-x", scope: { ...memoryScope, tenant_id: "other" }, memory_kind: "fact", name: "n", size_bytes: 1 },
      { kind: "memory_candidate_pending", turn: 2, candidate_id: "cand-1", source: "agent", name: "n" },
      { kind: "memory_candidate_rejected", turn: 3, candidate_id: "cand-1", source: "agent", name: "n", receipt_id: "r", reason_code: "dup" },
      { kind: "memory_candidate_pending", turn: 3, candidate_id: "cand-2", source: "agent", name: "n", receipt_id: "r2" },
      { kind: "memory_retrieval_result", hits: [{ score: 1, why: "w", record: recalled }] },
    ] as SessionEvent[]) await sessionLog.append("session-1", event)
    const kernelJournal = new InMemoryKernelJournal()
    await kernelJournal.compareAndAppend("op-1", undefined, { step_seq: 0, record_digest: "d0", record_bytes: new Uint8Array([0]) })
    await kernelJournal.compareAndAppend("op-1", "d0", { step_seq: 1, record_digest: "d1", record_bytes: new Uint8Array([1]) })
    await kernelJournal.compareAndInstallCheckpoint("op-1", undefined, "d1", {
      checkpoint_id: "cp-1", through_step_seq: 1, state_digest: "sha256:state", checkpoint_bytes: new Uint8Array([9]),
    })
    return { sessionLog, kernelJournal }
  }

  it("reads refs and verification bytes from SessionLog and KernelJournal", async () => {
    const { sessionLog, kernelJournal } = await seed()
    const reader = await SessionEvidenceReader.read({
      operationId: "op-1",
      sessionId: "session-1",
      scope: sourceScope,
      sessionLog,
      kernelJournal,
      sidecar: { knowledgeProvenance: ({ entryId }) => `kb:${entryId}` },
    })
    const snapshot = reader.snapshot
    const checkpoint = await kernelJournal.latestCheckpoint("op-1")

    expect(snapshot.asOf).toEqual({ sessionLogSeq: 7, journalStepSeq: 1, checkpointOrdinal: checkpoint!.ordinal })
    expect(snapshot.refs.filter(item => item.domain !== "session_event").map(item => `${item.domain}/${item.refId}`)).toEqual([
      "checkpoint/cp-1",
      "context_binding/effect-1",
      "journal_record/0",
      "journal_record/1",
      "knowledge_entry/kb-1@sha256:kb",
      "memory_candidate/cand-1",
      "memory_candidate/cand-2",
      "memory_record/mem-1",
      "provider_attempt/effect-1#1",
    ])
    expect(reader.byRefId("knowledge_entry", "kb-1@sha256:kb")?.provenance).toBe("kb:kb-1")
    expect(reader.byRefId("memory_candidate", "cand-1")).toMatchObject({ settlement: "rejected", version: "r" })
    expect(reader.byRefId("memory_candidate", "cand-2")).toMatchObject({ settlement: "pending", version: "r2" })
    expect(reader.byRefId("memory_record", "mem-1")).toMatchObject({
      settlement: "accepted",
      sizeBytes: 12,
      provenance: "agent/observed",
      evidenceRefs: ["turn:1", "tool:t-1"],
      retention: { pinned: false, observedTtlDays: 7 },
    })
    expect(reader.byDigest("sha256:state").map(item => item.refId)).toEqual(["cp-1"])

    const verification = snapshot.verification
    expect(verification.journal.map(bytes => [...bytes])).toEqual([[0], [1]])
    expect(verification.checkpoints?.map(bytes => [...bytes])).toEqual([[9]])
    expect(JSON.parse(new TextDecoder().decode(verification.sessionLogs![0][1]))).toMatchObject({ kind: "provider_attempt" })

    // One session_event ref per logged event, digesting exactly the verification bytes.
    const events = reader.refs("session_event")
    expect(events.map(item => item.refId)).toEqual(["0", "1", "2", "3", "4", "5", "6", "7"])
    verification.sessionLogs![0].forEach((bytes, seq) => {
      expect(reader.byRefId("session_event", String(seq))).toMatchObject({
        contentDigest: `sha256:${createHash("sha256").update(bytes).digest("hex")}`,
        sizeBytes: bytes.byteLength,
        payloadRef: `sessionlog:${seq}`,
      })
    })

    // The only gap is the excluded cross-tenant memory write.
    expect(snapshot.completeness).toBe("partial")
    expect(snapshot.diagnostics.map(item => `${item.code}:${item.refId}`)).toEqual(["memory_scope_mismatch:mem-x"])
  })

  it("degrades without provenance, journal, or a readable session log", async () => {
    const { sessionLog } = await seed()
    const withoutJournal = await readSessionEvidence({ operationId: "op-1", sessionId: "session-1", scope: sourceScope, sessionLog })
    expect(withoutJournal.completeness).toBe("partial")
    expect(withoutJournal.diagnostics.map(item => item.code)).toEqual(expect.arrayContaining([
      "journal_not_provided", "knowledge_provenance_missing",
    ]))

    const broken = { read: async () => { throw new Error("disk gone") }, append: async () => 0, latestSeq: async () => -1 }
    const unreadable = await readSessionEvidence({ operationId: "op-1", sessionId: "session-1", sessionLog: broken })
    expect(unreadable.completeness).toBe("unavailable")
    expect(unreadable.asOf).toBeUndefined()
    expect(unreadable.diagnostics.map(item => item.code)).toEqual(["session_log_unreadable", "journal_not_provided", "evidence_empty"])
  })

  it("flags a pruned journal prefix that no checkpoint covers", async () => {
    const kernelJournal = {
      readFrom: async () => [{ step_seq: 5, record_digest: "d5", record_bytes: new Uint8Array([5]) }],
      latestCheckpoint: async () => undefined,
    } as unknown as KernelJournal
    const snapshot = await readSessionEvidence({
      operationId: "op-1", sessionId: "session-1", sessionLog: new InMemorySessionLog(), kernelJournal,
    })
    expect(snapshot.completeness).toBe("partial")
    expect(snapshot.asOf).toEqual({ sessionLogSeq: -1, journalStepSeq: 5 })
    expect(snapshot.diagnostics.map(item => item.code)).toEqual(["journal_prefix_pruned"])
  })

  it("refines pending candidates from the sink's receipt without submitting", async () => {
    const { sessionLog, kernelJournal } = await seed()
    const receipts: Record<string, MemoryCandidateReceipt> = {
      "cand-1": { status: "accepted", candidate_id: "cand-1", receipt_id: "r9", record_ref: "mem:9", digest: "sha256:m9" },
      "cand-2": { status: "accepted", candidate_id: "cand-2", receipt_id: "r3", record_ref: "mem:3", digest: "sha256:m3" },
    }
    const queried: string[] = []
    const sidecar = {
      knowledgeProvenance: () => "kb",
      memoryCandidateReceipt: ({ agentId, candidateId }: { agentId: string, candidateId: string }) => {
        queried.push(`${agentId}/${candidateId}`)
        return receipts[candidateId]
      },
      submit: () => { throw new Error("reader must not submit") },
    }
    const reader = await SessionEvidenceReader.read({
      operationId: "op-1", sessionId: "session-1", scope: sourceScope, sessionLog, kernelJournal, sidecar,
    })

    expect(queried).toEqual(["agent-1/cand-1", "agent-1/cand-2"])
    expect(reader.byRefId("memory_candidate", "cand-2")).toMatchObject({
      settlement: "accepted", version: "r3", contentDigest: "sha256:m3", payloadRef: "mem:3",
    })
    // The log recorded cand-1 as rejected; a contradicting receipt is reported, not applied.
    expect(reader.byRefId("memory_candidate", "cand-1")).toMatchObject({ settlement: "rejected", version: "r" })
    expect(reader.snapshot.diagnostics.map(item => `${item.code}:${item.refId}`)).toEqual([
      "memory_scope_mismatch:mem-x", "memory_candidate_receipt_conflict:cand-1",
    ])

    const unscoped = await readSessionEvidence({
      operationId: "op-1", sessionId: "session-1", sessionLog, kernelJournal, sidecar,
    })
    expect(unscoped.diagnostics.map(item => item.code)).toContain("memory_candidate_agent_unscoped")
    expect(unscoped.refs.find(item => item.refId === "cand-2")?.settlement).toBe("pending")
  })

  it("rejects a receipt that renames the logged answer without changing it", async () => {
    const sessionLog = new InMemorySessionLog()
    await sessionLog.append("session-1", {
      kind: "memory_candidate_pending", turn: 1, candidate_id: "cand-1", source: "session_extraction", name: "n", receipt_id: "r1",
    })
    const snapshot = await readSessionEvidence({
      operationId: "op-1", sessionId: "session-1", scope: sourceScope, sessionLog,
      sidecar: { memoryCandidateReceipt: () => ({ status: "pending", candidate_id: "cand-1", receipt_id: "r2" }) },
    })
    expect(snapshot.refs.find(item => item.refId === "cand-1")).toMatchObject({ settlement: "pending", version: "r1" })
    expect(snapshot.diagnostics.map(item => `${item.code}:${item.refId}`)).toContain("memory_candidate_receipt_conflict:cand-1")
  })

  it("links an accepted candidate to the record this log wrote under its record_ref", async () => {
    const sessionLog = new InMemorySessionLog()
    for (const event of [
      { kind: "memory_candidate_pending", turn: 1, candidate_id: "cand-1", source: "session_extraction", name: "n", receipt_id: "r1" },
      { kind: "memory_candidate_pending", turn: 1, candidate_id: "cand-2", source: "session_extraction", name: "m", receipt_id: "r2" },
      { kind: "memory_written", turn: 2, record_id: "rec-1", scope: memoryScope, memory_kind: "project", name: "n", size_bytes: 3 },
    ] as SessionEvent[]) await sessionLog.append("session-1", event)
    const receipts: Record<string, MemoryCandidateReceipt> = {
      "cand-1": { status: "accepted", candidate_id: "cand-1", receipt_id: "r9", record_ref: "rec-1", digest: "sha256:r" },
      // Accepted after the run: its record is not in this log, so there is nothing to link to.
      "cand-2": { status: "accepted", candidate_id: "cand-2", receipt_id: "r8", record_ref: "rec-later", digest: "sha256:l" },
    }
    const reader = await SessionEvidenceReader.read({
      operationId: "op-1", sessionId: "session-1", scope: sourceScope, sessionLog,
      sidecar: { memoryCandidateReceipt: ({ candidateId }) => receipts[candidateId] },
    })
    const record = reader.byRefId("memory_record", "rec-1")!
    expect(reader.byRefId("memory_candidate", "cand-1")?.links).toEqual([
      { relation: "settled_as", domain: "memory_record", refId: "rec-1" },
    ])
    expect(reader.linking(record, "settled_as").map(item => item.refId)).toEqual(["cand-1"])
    expect(reader.byRefId("memory_candidate", "cand-2")).toMatchObject({ settlement: "accepted", payloadRef: "rec-later" })
    expect(reader.byRefId("memory_candidate", "cand-2")?.links).toBeUndefined()
    expect(reader.snapshot.diagnostics.map(item => item.code)).not.toContain("evidence_link_unresolved")
  })
})

describe("knowledge evidence identity", () => {
  const memoryScope = { tenant_id: "tenant-1", namespace: "default" }
  const sourceScope = { agentId: "agent-1", tenantId: "tenant-1", namespace: "default" }
  const digest = (name: string) => `sha256:${name.padEnd(64, "0")}`

  function prepared(turn: number, entries: Array<{ key: string, digest: string }>): SessionEvent {
    return {
      kind: "context_prepared",
      turn,
      effect_id: `effect-${turn}`,
      preparation: {
        binding: { digest: `sha256:binding-${turn}` },
        state: {
          knowledge: entries.map((entry, ordinal) => ({
            entry_id: `knowledge:${ordinal}:${entry.key}:${entry.digest}`,
            content_digest: entry.digest,
            source: "knowledge",
            ordinal,
          })),
        },
      },
    } as unknown as SessionEvent
  }

  it("identifies an entry by key and content, not by its shifting position", () => {
    expect(knowledgeIdentity(`knowledge:3:skill:debugging:${digest("a")}`, digest("a"))).toEqual({
      refId: `skill:debugging@${digest("a")}`, key: "skill:debugging", origin: "skill",
    })
    expect(knowledgeIdentity(`knowledge:0:unkeyed:${digest("b")}`, digest("b"))).toEqual({
      refId: `unkeyed@${digest("b")}`, key: "unkeyed",
    })
    // Not the kernel's shape: the whole id is the key.
    expect(knowledgeIdentity("kb-1", "sha256:kb").refId).toBe("kb-1@sha256:kb")
  })

  it("links recalled knowledge to its memory record and derives origin without a sidecar", async () => {
    const sessionLog = new InMemorySessionLog()
    const record = {
      record_id: "tenant-1:default:project:note",
      scope: memoryScope,
      pinned: false,
      provenance: { author: "extraction", trust: "untrusted", evidence_refs: [] },
    }
    for (const event of [
      { kind: "memory_retrieval_result", hits: [{ score: 1, why: "w", record }] },
      prepared(1, [
        { key: "skill:debugging", digest: digest("s") },
        { key: `memory:${record.record_id}`, digest: digest("m") },
      ]),
      // A sweep removed the skill: the memory entry moved from ordinal 1 to ordinal 0.
      prepared(2, [{ key: `memory:${record.record_id}`, digest: digest("m") }]),
    ] as SessionEvent[]) await sessionLog.append("session-1", event)

    const reader = await SessionEvidenceReader.read({
      operationId: "op-1", sessionId: "session-1", scope: sourceScope, sessionLog, kernelJournal: new InMemoryKernelJournal(),
    })
    const knowledge = reader.refs("knowledge_entry")
    expect(knowledge.map(item => item.refId)).toEqual([
      `memory:${record.record_id}@${digest("m")}`,
      `skill:debugging@${digest("s")}`,
    ])
    expect(knowledge[0]).toMatchObject({
      provenance: "memory",
      links: [{ relation: "recalled_from", domain: "memory_record", refId: record.record_id }],
    })
    const memory = reader.byRefId("memory_record", record.record_id)!
    expect(reader.linked(knowledge[0], "recalled_from")).toEqual([memory])
    expect(reader.linking(memory).map(item => item.refId)).toEqual([knowledge[0].refId])
    expect(reader.snapshot.diagnostics.map(item => item.code)).not.toContain("knowledge_provenance_missing")
  })

  it("reports recalled knowledge whose memory record is missing from the log", async () => {
    const sessionLog = new InMemorySessionLog()
    await sessionLog.append("session-1", prepared(1, [{ key: "memory:gone", digest: digest("g") }]))
    const snapshot = await readSessionEvidence({
      operationId: "op-1", sessionId: "session-1", scope: sourceScope, sessionLog, kernelJournal: new InMemoryKernelJournal(),
    })
    expect(snapshot.completeness).toBe("partial")
    expect(snapshot.diagnostics.map(item => `${item.code}:${item.refId}`)).toEqual([
      `evidence_link_unresolved:memory:gone@${digest("g")}`,
    ])
  })

  it("rejects malformed links", () => {
    expect(() => createSharedEvidence({
      operationId: "op-1",
      scope: inputScope,
      verification: { journal },
      refs: [ref({ links: [{ relation: "copied_from" as never, domain: "memory_record", refId: "m" }] })],
    })).toThrow("evidence link.relation is invalid")
  })
})
