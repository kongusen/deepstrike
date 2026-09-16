import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"

import { getKernel } from "../src/kernel.js"

/**
 * P7-S1 / F16: this suite consumes the ENTIRE tests/fixtures/kernel-wire directory via
 * readdir — a fixture that is neither executed nor charter-exempted below fails the sweep
 * test. The charter lists the only fixtures not executed here, each with the surface that
 * does enforce it. An empty charter is the default; every entry needs a reason.
 */

const FIXTURE_DIR = join(process.cwd(), "../tests/fixtures/kernel-wire")

type Json = Record<string, unknown>

function readFixture(name: string): Json {
  return JSON.parse(readFileSync(join(FIXTURE_DIR, name), "utf8")) as Json
}

function listFixtures(): string[] {
  return readdirSync(FIXTURE_DIR).filter((name) => name.endsWith(".json")).sort()
}

function byPrefix(names: string[], prefix: string): string[] {
  return names.filter((name) => name.startsWith(prefix))
}

type RejectedFault = { code?: string; message?: string }

/** Decode-stage deaths surface from the binding as malformed_envelope; policy violations
 *  from config resolution surface as invalid_config (binding.rs rejection_fault). Anything
 *  else (prepared/replayed/lifecycle faults) proves the bytes were accepted at the wire. */
function prepareFault(kernel: { prepare(input: string): { status: string; faultJson?: string } }, envelope: unknown): { status: string; fault: RejectedFault } {
  const prepared = kernel.prepare(JSON.stringify(envelope))
  if (prepared.status !== "rejected") return { status: prepared.status, fault: {} }
  return { status: prepared.status, fault: JSON.parse(prepared.faultJson ?? "{}") as RejectedFault }
}

/** expect kind → marker substring guaranteed present in the rejection message. */
const REJECT_MARKERS: Record<string, string> = {
  unknown_field: "unknown field",
  unknown_variant: "unknown variant",
  missing_field: "missing field",
  invalid_scalar: "wire scalar rejected",
}

/** Envelope-owned facts a business input must never repeat (mirror of core tests.rs). */
const BANNED_INPUT_KEYS = [
  "operation_id", "event_id", "now_ms", "observed_at_ms", "session_id",
  "parent_session_id", "agent_id", "memory_path", "path", "file_path", "spool_dir",
]

/** Host-owned facts a configuration fixture must never carry (mirror of core config.rs). */
const BANNED_CONFIG_KEYS = [
  "memory_path", "spool_dir", "tokenizer", "host_effect_retry_attempts",
  "session_id", "api_key", "endpoint",
]

function allKeys(value: unknown, out: Set<string>): void {
  if (Array.isArray(value)) {
    for (const item of value) allKeys(item, out)
  } else if (value && typeof value === "object") {
    for (const [key, child] of Object.entries(value)) {
      out.add(key)
      allKeys(child, out)
    }
  }
}

function expectedLifecycleFor(fixture: Json): string {
  const links = fixture.links as Array<{ step: Json }>
  const disposition = links[links.length - 1].step.disposition as Json
  if (disposition.kind !== "terminal") return "running"
  const terminal = disposition.terminal as Json
  if (terminal.kind === "cancelled") return "cancelled"
  if (terminal.kind === "failed") return "failed"
  return "completed"
}

describe("Node canonical ABI fixtures (full-directory sweep)", () => {
  const names = listFixtures()
  const native = getKernel()

  // -- family partitions -----------------------------------------------------

  const lifecycle = byPrefix(names, "golden_lifecycle_")
  const recordChain = byPrefix(names, "golden_record_chain")
  const recordGenesis = byPrefix(names, "golden_record_genesis")
  const configResolved = byPrefix(names, "golden_config_resolved")
  const configRejects = byPrefix(names, "golden_config_reject_")
  const configRejectsDefaultLimits = configRejects.filter(
    (name) => !("bootstrap_limits" in readFixture(name)),
  )
  const inputs = byPrefix(names, "input_")
  const envelopeRejects = byPrefix(names, "reject_").filter(
    (name) => !name.startsWith("reject_checkpoint_") && !name.startsWith("reject_transaction_"),
  )

  /** Charter: fixtures NOT executed here, each with the enforcing surface.
   *  Families: prefix entries (ending in "_"); individual files: exact names without ".json". */
  const CHARTER: Record<string, string> = {
    reject_checkpoint_:
      "checkpoint blobs carry the §12 taxonomy, not the envelope decode boundary; enforced by " +
      "core checkpoint::tests and by this SDK's restore rejection tests (canonical-binding.test.ts)",
    reject_transaction_:
      "§7.13 faults from a well-formed envelope the transaction refuses (checkpoint_required " +
      "needs a full journal tail); enforced by core driver §12.3 tests",
    golden_checkpoint_:
      "checkpoint candidate/rebase/restore snapshots are produced and pinned by core " +
      "driver/tests.rs (J1: canonical bytes are core-owned); the SDK restore path is covered by " +
      "canonical-binding.test.ts restore-in-place tests",
    golden_record_canonical_bytes:
      "canonical byte vectors are core-owned (J1); asserted in core record.rs " +
      "golden_canonical_bytes_vectors",
    golden_record_transition:
      "its step is a synthetic record.rs helper step and a resolve_effect envelope is not " +
      "drivable standalone (no pending effect exists by design); pinned by core record.rs " +
      "golden_transition_record. The normalisation/chain-linkage surface is covered by the " +
      "genesis/chain drives below",
    golden_terminal_:
      "bare terminal wire shapes are owned by core terminal.rs; the SDK asserts terminals " +
      "end-to-end via terminalJson() deep-equality in the lifecycle drive below",
    golden_config_reject_limits_widen_bootstrap:
      "fixture requires fixture-supplied bootstrap limits; SDK bindings construct with defaults. " +
      "Enforced by core config.rs with the fixture's limits",
  }

  const charterCovers = (name: string): boolean => {
    const base = name.replace(/\.json$/, "")
    return Object.keys(CHARTER).some((key) => (key.endsWith("_") ? name.startsWith(key) : base === key))
  }

  // -- the sweep itself: nothing in the directory may go unclassified ---------

  it("classifies every fixture in the directory (zero undocumented exemptions)", () => {
    const executed = new Set([
      ...lifecycle,
      ...recordChain,
      ...recordGenesis,
      ...configResolved,
      ...configRejectsDefaultLimits,
      ...inputs,
      ...envelopeRejects,
    ])
    const unclassified = names.filter((name) => !executed.has(name) && !charterCovers(name))
    expect(unclassified).toEqual([])

    // every charter entry must still match something — a stale charter entry is drift
    const stale = Object.keys(CHARTER).filter(
      (key) =>
        !names.some((name) =>
          key.endsWith("_") ? name.startsWith(key) : name.replace(/\.json$/, "") === key,
        ),
    )
    expect(stale).toEqual([])

    // the charter must stay narrow: bootstrap_limits-carrying config rejects are the only
    // data-driven exemptions allowed beyond the list above
    for (const name of configRejects) {
      if (!configRejectsDefaultLimits.includes(name) && !CHARTER[name.replace(/\.json$/, "")]) {
        throw new Error(`${name}: unchartered bootstrap-limits exemption`)
      }
    }
  })

  // -- golden lifecycles: drive every link, pin digests and the terminal ------

  it.each(lifecycle.map((name) => [name]))("drives %s to its declared digests", (name) => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const links = fixture.links as Array<{ envelope: Json; record: Json; step: Json }>

    links.forEach((link, index) => {
      expect(link.envelope).not.toHaveProperty("abi_version")
      const prepared = kernel.prepare(JSON.stringify(link.envelope))
      if (prepared.status !== "prepared") {
        throw new Error(`${name} link ${index}: expected prepared, got ${prepared.status}`)
      }
      expect(JSON.parse(prepared.plannedStepJson)).toEqual(link.step)
      // J1 pass-through: the real kernel reproduces the pinned record byte for byte
      expect(Buffer.from(prepared.recordBytes).toString("utf8")).toBe(JSON.stringify(link.record))
      const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
      expect(committed.stepSeq).toBe(String(index))
      expect(committed.recordDigest).toBe(link.record.record_digest as string)
    })

    expect(kernel.lifecycle()).toBe(expectedLifecycleFor(fixture))

    const last = links[links.length - 1].step.disposition as Json
    const terminalJson = kernel.terminalJson()
    if (last.kind === "terminal") {
      expect(terminalJson).toBeDefined()
      expect(JSON.parse(terminalJson as string)).toEqual(last.terminal)
    } else {
      expect(terminalJson ?? null).toBeNull()
    }
  })

  // -- record goldens: normalisation + chain linkage through the real kernel --

  /** The fixture records pin synthetic record.rs helper steps, so step_digest/record_digest (and
   *  the previous_record_digest chain built on them) are core-unit pins, not kernel-drive pins.
   *  What a full-directory drive can and must reproduce: the normalised input (canonical_input +
   *  input_digest) and the envelope identity fields; hash-chain linkage is asserted separately
   *  against the real digests the kernel produces. */
  function assertRecordSansChainDigests(produced: Json, pinned: Json): void {
    const strip = (record: Json): Json => {
      const copy = { ...record }
      delete copy.record_digest
      delete copy.step_digest
      delete copy.previous_record_digest
      return copy
    }
    expect(strip(produced)).toEqual(strip(pinned))
    expect(produced.record_digest).toBeDefined()
    expect(produced.step_digest).toBeDefined()
  }

  it.each(recordChain.map((name) => [name]))("drives %s pinning normalisation and linkage", (name) => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const links = fixture.links as Array<{ envelope: Json; record: Json }>
    expect(links.length).toBeGreaterThan(0)

    let previousDigest: string | null = null
    links.forEach((link, index) => {
      const prepared = kernel.prepare(JSON.stringify(link.envelope))
      if (prepared.status !== "prepared") {
        throw new Error(`${name} link ${index}: expected prepared, got ${prepared.status}`)
      }
      const produced = JSON.parse(Buffer.from(prepared.recordBytes).toString("utf8")) as Json
      assertRecordSansChainDigests(produced, link.record)
      expect(produced.previous_record_digest).toBe(previousDigest)
      const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
      expect(committed.recordDigest).toBe(produced.record_digest as string)
      previousDigest = committed.recordDigest
    })
  })

  it.each(recordGenesis.map((name) => [name]))("drives %s as a fresh genesis", (name) => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const prepared = kernel.prepare(JSON.stringify(fixture.envelope))
    if (prepared.status !== "prepared") {
      throw new Error(`${name}: expected prepared, got ${prepared.status}`)
    }
    const produced = JSON.parse(Buffer.from(prepared.recordBytes).toString("utf8")) as Json
    assertRecordSansChainDigests(produced, fixture.record as Json)
    expect(produced.previous_record_digest).toBeNull()
    const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
    expect(committed.recordDigest).toBe(produced.record_digest as string)
  })

  // -- config goldens ---------------------------------------------------------

  it.each(configResolved.map((name) => [name]))("accepts %s at the wire", (name) => {
    // the frozen ResolvedOperationConfig comparison is core-side (config.rs); the SDK asserts
    // the fixture config is accepted by a fresh kernel
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const envelope = {
      operation_id: "op-fixture-config",
      input_id: `in-${name}`,
      observed_at_ms: "1753747200000",
      input: { kind: "configure_operation", config: fixture.config },
    }
    const prepared = kernel.prepare(JSON.stringify(envelope))
    expect(prepared.status).toBe("prepared")
  })

  it.each(configRejectsDefaultLimits.map((name) => [name]))(
    "rejects %s with invalid_config",
    (name) => {
      const fixture = readFixture(name)
      // resolution-stage rejections map to fault codes per binding.rs rejection_fault:
      // policy_violation → invalid_config; collection_too_large currently → malformed_envelope
      // (asymmetry under constitution review — both are §7.7 resolution-stage rejections)
      const expectedCode =
        fixture.expect === "policy_violation" ? "invalid_config" : "malformed_envelope"
      const kernel = new native.CanonicalKernel()
      const envelope = {
        operation_id: "op-fixture-config",
        input_id: `in-${name}`,
        observed_at_ms: "1753747200000",
        input: { kind: "configure_operation", config: fixture.config },
      }
      const { status, fault } = prepareFault(kernel, envelope)
      expect(status).toBe("rejected")
      expect(fault.code).toBe(expectedCode)
    },
  )

  // -- input goldens: wire acceptance + banned-key scan -----------------------

  it.each(inputs.map((name) => [name]))("accepts %s at the decode boundary", (name) => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const { status, fault } = prepareFault(kernel, fixture)
    // a decode-stage death is the only failure mode this test tolerates nothing of;
    // lifecycle/authority faults still prove the bytes were wire-valid
    if (status === "rejected" && fault.code === "malformed_envelope") {
      throw new Error(`${name}: decode-stage rejection: ${fault.message}`)
    }
  })

  it("no input fixture repeats envelope-owned facts", () => {
    for (const name of inputs) {
      const keys = new Set<string>()
      allKeys((readFixture(name).input as Json) ?? {}, keys)
      for (const banned of BANNED_INPUT_KEYS) {
        if (keys.has(banned)) throw new Error(`${name}: business input repeats ${banned}`)
      }
    }
  })

  it("no config fixture carries host-owned facts", () => {
    for (const name of [...byPrefix(names, "input_configure_"), ...byPrefix(names, "golden_config_")]) {
      const keys = new Set<string>()
      allKeys(readFixture(name), keys)
      for (const banned of BANNED_CONFIG_KEYS) {
        if (keys.has(banned)) throw new Error(`${name}: config fixture carries ${banned}`)
      }
    }
  })

  // -- rejection fixtures: fail closed with the declared marker ---------------

  it.each(envelopeRejects.map((name) => [name]))("rejects %s fail-closed", (name) => {
    const fixture = readFixture(name)
    const expected = fixture.expect as string
    const marker = REJECT_MARKERS[expected]
    if (!marker) throw new Error(`${name}: no marker registered for expect kind ${expected}`)

    const kernel = new native.CanonicalKernel()
    const { status, fault } = prepareFault(kernel, fixture.envelope)
    expect(status).toBe("rejected")
    expect(fault.code).toBe("malformed_envelope")
    if (!(fault.message ?? "").includes(marker)) {
      throw new Error(`${name}: rejection message must carry the ${expected} marker, got: ${fault.message}`)
    }
  })

  it("envelope reject fixtures cover the required kinds", () => {
    const kinds = new Set(envelopeRejects.map((name) => readFixture(name).expect as string))
    expect(kinds).toContain("unknown_field")
    expect(kinds).toContain("unknown_variant")
  })
})
