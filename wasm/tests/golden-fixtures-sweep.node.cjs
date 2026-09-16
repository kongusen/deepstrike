/**
 * P7-S1 / F16: WASM full-directory sweep of tests/fixtures/kernel-wire against the REAL wasm
 * binding (pkg-node). This intentionally does NOT run under jest: jest maps
 * `@deepstrike/wasm-kernel` to tests/__mocks__/kernel.ts, and a mock can never enforce wire
 * discipline. Run via `npm run test:canonical-binding` (builds pkg-node first).
 *
 * A fixture that is neither executed nor charter-exempted below fails the sweep. The charter
 * lists the only fixtures not executed here, each with the surface that does enforce it.
 */
const assert = require("node:assert/strict")
const { readdirSync, readFileSync } = require("node:fs")
const { join } = require("node:path")

const native = require("../pkg-node/deepstrike_wasm.js")

const FIXTURE_DIR = join(__dirname, "../../tests/fixtures/kernel-wire")

const readFixture = (name) => JSON.parse(readFileSync(join(FIXTURE_DIR, name), "utf8"))
const names = readdirSync(FIXTURE_DIR).filter((name) => name.endsWith(".json")).sort()
const byPrefix = (prefix) => names.filter((name) => name.startsWith(prefix))

/** Decode-stage deaths surface from the binding as malformed_envelope; policy violations from
 *  config resolution surface as invalid_config (binding.rs rejection_fault). Anything else
 *  (prepared/replayed/lifecycle faults) proves the bytes were accepted at the wire. */
function prepareFault(kernel, envelope) {
  const prepared = kernel.prepare(JSON.stringify(envelope))
  if (prepared.status !== "rejected") return { status: prepared.status, fault: {} }
  return { status: prepared.status, fault: JSON.parse(prepared.faultJson ?? "{}") }
}

/** expect kind → marker substring guaranteed present in the rejection message. */
const REJECT_MARKERS = {
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

function allKeys(value, out) {
  if (Array.isArray(value)) {
    for (const item of value) allKeys(item, out)
  } else if (value && typeof value === "object") {
    for (const [key, child] of Object.entries(value)) {
      out.add(key)
      allKeys(child, out)
    }
  }
}

function expectedLifecycleFor(fixture) {
  const links = fixture.links
  const disposition = links[links.length - 1].step.disposition
  if (disposition.kind !== "terminal") return "running"
  const terminal = disposition.terminal
  if (terminal.kind === "cancelled") return "cancelled"
  if (terminal.kind === "failed") return "failed"
  return "completed"
}

// -- family partitions ---------------------------------------------------------

const lifecycle = byPrefix("golden_lifecycle_")
const recordChain = byPrefix("golden_record_chain")
const recordGenesis = byPrefix("golden_record_genesis")
const configResolved = byPrefix("golden_config_resolved")
const configRejects = byPrefix("golden_config_reject_")
const configRejectsDefaultLimits = configRejects.filter(
  (name) => !("bootstrap_limits" in readFixture(name)),
)
const inputs = byPrefix("input_")
const envelopeRejects = byPrefix("reject_").filter(
  (name) => !name.startsWith("reject_checkpoint_") && !name.startsWith("reject_transaction_"),
)

/** Charter: fixtures NOT executed here, each with the enforcing surface.
 *  Families: prefix entries (ending in "_"); individual files: exact names without ".json". */
const CHARTER = {
  reject_checkpoint_:
    "checkpoint blobs carry the §12 taxonomy, not the envelope decode boundary; enforced by " +
    "core checkpoint::tests and by this SDK's restore rejection tests (canonical-binding.node.cjs)",
  reject_transaction_:
    "§7.13 faults from a well-formed envelope the transaction refuses (checkpoint_required " +
    "needs a full journal tail); enforced by core driver §12.3 tests",
  golden_checkpoint_:
    "checkpoint candidate/rebase/restore snapshots are produced and pinned by core " +
    "driver/tests.rs (J1: canonical bytes are core-owned); the SDK restore path is covered by " +
    "canonical-binding.node.cjs restore-in-place assertions",
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

const charterCovers = (name) => {
  const base = name.replace(/\.json$/, "")
  return Object.keys(CHARTER).some((key) => (key.endsWith("_") ? name.startsWith(key) : base === key))
}

let checks = 0
const check = (label, fn) => {
  fn()
  checks += 1
  console.log(`ok ${checks} - ${label}`)
}

// -- the sweep itself: nothing in the directory may go unclassified ------------

check("every fixture is executed or charter-exempted (zero undocumented exemptions)", () => {
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
  assert.deepEqual(unclassified, [])

  // every charter entry must still match something — a stale charter entry is drift
  const stale = Object.keys(CHARTER).filter(
    (key) =>
      !names.some((name) =>
        key.endsWith("_") ? name.startsWith(key) : name.replace(/\.json$/, "") === key,
      ),
  )
  assert.deepEqual(stale, [])

  // the charter must stay narrow: bootstrap_limits-carrying config rejects are the only
  // data-driven exemptions allowed beyond the list above
  for (const name of configRejects) {
    if (!configRejectsDefaultLimits.includes(name)) {
      assert.ok(
        CHARTER[name.replace(/\.json$/, "")],
        `${name}: unchartered bootstrap-limits exemption`,
      )
    }
  }
})

// -- golden lifecycles: drive every link, pin digests, records, terminal --------

for (const name of lifecycle) {
  check(`drives ${name} to its declared digests`, () => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const links = fixture.links

    links.forEach((link, index) => {
      assert.ok(!("abi_version" in link.envelope), `${name} link ${index}: abi_version leaked`)
      const prepared = kernel.prepare(JSON.stringify(link.envelope))
      assert.equal(prepared.status, "prepared", `${name} link ${index}`)
      assert.deepEqual(JSON.parse(prepared.plannedStepJson), link.step)
      // J1 pass-through: the real kernel reproduces the pinned record byte for byte
      assert.equal(Buffer.from(prepared.recordBytes).toString("utf8"), JSON.stringify(link.record))
      const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
      assert.equal(committed.stepSeq, String(index))
      assert.equal(committed.recordDigest, link.record.record_digest)
    })

    assert.equal(kernel.lifecycle(), expectedLifecycleFor(fixture))

    const last = links[links.length - 1].step.disposition
    const terminalJson = kernel.terminalJson()
    if (last.kind === "terminal") {
      assert.ok(terminalJson, `${name}: terminal expected`)
      assert.deepEqual(JSON.parse(terminalJson), last.terminal)
    } else {
      assert.equal(terminalJson ?? null, null)
    }
  })
}

// -- record goldens: normalisation + chain linkage through the real kernel -----

/** The fixture records pin synthetic record.rs helper steps, so step_digest/record_digest (and
 *  the previous_record_digest chain built on them) are core-unit pins, not kernel-drive pins.
 *  What a full-directory drive can and must reproduce: the normalised input (canonical_input +
 *  input_digest) and the envelope identity fields; hash-chain linkage is asserted separately
 *  against the real digests the kernel produces. */
function assertRecordSansChainDigests(produced, pinned) {
  const strip = (record) => {
    const copy = { ...record }
    delete copy.record_digest
    delete copy.step_digest
    delete copy.previous_record_digest
    return copy
  }
  assert.deepEqual(strip(produced), strip(pinned))
  assert.ok(produced.record_digest)
  assert.ok(produced.step_digest)
}

for (const name of recordChain) {
  check(`drives ${name} pinning normalisation and linkage`, () => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const links = fixture.links
    assert.ok(links.length > 0)

    let previousDigest = null
    links.forEach((link, index) => {
      const prepared = kernel.prepare(JSON.stringify(link.envelope))
      assert.equal(prepared.status, "prepared", `${name} link ${index}`)
      const produced = JSON.parse(Buffer.from(prepared.recordBytes).toString("utf8"))
      assertRecordSansChainDigests(produced, link.record)
      assert.equal(produced.previous_record_digest, previousDigest)
      const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
      assert.equal(committed.recordDigest, produced.record_digest)
      previousDigest = committed.recordDigest
    })
  })
}

for (const name of recordGenesis) {
  check(`drives ${name} as a fresh genesis`, () => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const prepared = kernel.prepare(JSON.stringify(fixture.envelope))
    assert.equal(prepared.status, "prepared", name)
    const produced = JSON.parse(Buffer.from(prepared.recordBytes).toString("utf8"))
    assertRecordSansChainDigests(produced, fixture.record)
    assert.equal(produced.previous_record_digest, null)
    const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
    assert.equal(committed.recordDigest, produced.record_digest)
  })
}

// -- config goldens ------------------------------------------------------------

for (const name of configResolved) {
  check(`accepts ${name} at the wire`, () => {
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
    assert.equal(prepared.status, "prepared", name)
  })
}

for (const name of configRejectsDefaultLimits) {
  check(`rejects ${name} with the resolution-stage fault`, () => {
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
    assert.equal(status, "rejected", name)
    assert.equal(fault.code, expectedCode, `${name}: ${fault.message}`)
  })
}

// -- input goldens: wire acceptance + banned-key scan --------------------------

for (const name of inputs) {
  check(`accepts ${name} at the decode boundary`, () => {
    const fixture = readFixture(name)
    const kernel = new native.CanonicalKernel()
    const { status, fault } = prepareFault(kernel, fixture)
    // a decode-stage death is the only failure mode this test tolerates nothing of;
    // lifecycle/authority faults still prove the bytes were wire-valid
    assert.ok(
      status !== "rejected" || fault.code !== "malformed_envelope",
      `${name}: decode-stage rejection: ${fault.message}`,
    )
  })
}

check("no input fixture repeats envelope-owned facts", () => {
  for (const name of inputs) {
    const keys = new Set()
    allKeys(readFixture(name).input ?? {}, keys)
    for (const banned of BANNED_INPUT_KEYS) {
      assert.ok(!keys.has(banned), `${name}: business input repeats ${banned}`)
    }
  }
})

check("no config fixture carries host-owned facts", () => {
  for (const name of [...byPrefix("input_configure_"), ...byPrefix("golden_config_")]) {
    const keys = new Set()
    allKeys(readFixture(name), keys)
    for (const banned of BANNED_CONFIG_KEYS) {
      assert.ok(!keys.has(banned), `${name}: config fixture carries ${banned}`)
    }
  }
})

// -- rejection fixtures: fail closed with the declared marker ------------------

for (const name of envelopeRejects) {
  check(`rejects ${name} fail-closed`, () => {
    const fixture = readFixture(name)
    const marker = REJECT_MARKERS[fixture.expect]
    assert.ok(marker, `${name}: no marker registered for expect kind ${fixture.expect}`)

    const kernel = new native.CanonicalKernel()
    const { status, fault } = prepareFault(kernel, fixture.envelope)
    assert.equal(status, "rejected", name)
    assert.equal(fault.code, "malformed_envelope", `${name}: ${fault.message}`)
    assert.ok(
      (fault.message ?? "").includes(marker),
      `${name}: rejection message must carry the ${fixture.expect} marker, got: ${fault.message}`,
    )
  })
}

check("envelope reject fixtures cover the required kinds", () => {
  const kinds = new Set(envelopeRejects.map((name) => readFixture(name).expect))
  assert.ok(kinds.has("unknown_field"))
  assert.ok(kinds.has("unknown_variant"))
})

console.log(`golden-fixtures sweep: ${checks} checks passed over ${names.length} fixtures`)
