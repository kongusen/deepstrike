const assert = require("node:assert/strict")
const { readFileSync } = require("node:fs")
const { join } = require("node:path")

const native = require("../pkg-node/deepstrike_wasm.js")

const fixture = JSON.parse(
  readFileSync(
    join(__dirname, "../../tests/fixtures/kernel-wire/golden_lifecycle_agent_root.json"),
    "utf8",
  ),
)
const declarations = readFileSync(join(__dirname, "../pkg-node/deepstrike_wasm.d.ts"), "utf8")

assert.equal(native.kernelAbiVersion(), 3)
const kernel = new native.CanonicalKernel()

const prepared = kernel.prepare(JSON.stringify(fixture.links[0].envelope))
assert.equal(prepared.status, "prepared")
assert.equal(prepared.stepSeq, "0")
assert.equal(prepared.recordDigest, fixture.genesis_digest)
assert.ok(prepared.recordBytes instanceof Uint8Array)
assert.equal(
  Buffer.from(prepared.recordBytes).toString("utf8"),
  JSON.stringify(fixture.links[0].record),
)

const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
assert.equal(committed.stepSeq, "0")
assert.equal(committed.recordDigest, fixture.genesis_digest)
assert.equal(kernel.lifecycle(), "configured")
const replayed = kernel.prepare(JSON.stringify(fixture.links[0].envelope))
assert.equal(replayed.status, "replayed")
assert.equal(replayed.recordDigest, fixture.genesis_digest)

const rebuilt = new native.CanonicalKernel()
const restoreCost = rebuilt.restore(undefined, [prepared.recordBytes])
assert.equal(restoreCost.recordsBeforeCheckpoint, "1")
assert.equal(restoreCost.recordsAfterCheckpoint, "0")
assert.equal(rebuilt.lifecycle(), "configured")

const checkpoint = kernel.checkpointCandidate()
const identity = kernel
kernel.restore(checkpoint.checkpointBytes, [])
assert.equal(kernel, identity)
assert.equal(kernel.lifecycle(), "configured")

const rejected = new native.CanonicalKernel().prepare("{")
assert.equal(rejected.status, "rejected")
assert.equal(JSON.parse(rejected.faultJson).code, "malformed_envelope")

assert.equal(typeof native.CanonicalKernel.prototype.step, "undefined")
assert.equal(typeof native.KernelRuntime, "undefined")
assert.match(
  declarations,
  /export type CanonicalPreparation = \{ status: "prepared";.*recordBytes: Uint8Array;.*status: "rejected"; faultJson: string \};/,
)
// record_bytes is JsValue at the ABI boundary (fallible in-body decode — C1); the
// CanonicalRecordBytes alias remains the documented TypeScript surface.
assert.match(declarations, /export type CanonicalRecordBytes = Uint8Array\[\];/)
assert.match(declarations, /restore\(checkpoint_bytes: Uint8Array \| null \| undefined, record_bytes: any\): CanonicalRestoreCost;/)

// C1: bad record_bytes must fail closed WITHOUT bricking the WasmRefCell handle.
// tsify/from_wasm_abi throw_str used to skip destructor release → permanent "recursive use".
const brickProbe = new native.CanonicalKernel()
assert.equal(brickProbe.lifecycle(), "created")
assert.throws(() => brickProbe.restore(undefined, undefined), /record_bytes|Array|Uint8Array|TypeError/i)
assert.equal(
  brickProbe.lifecycle(),
  "created",
  "bad restore args must not leak the WasmRefCell borrow (handle stays usable)",
)
assert.throws(() => brickProbe.restore(undefined, null), /record_bytes|Array|Uint8Array/i)
assert.equal(brickProbe.lifecycle(), "created")
