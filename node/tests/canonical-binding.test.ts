import { readFileSync } from "node:fs"
import { join } from "node:path"

import { getKernel } from "../src/kernel.js"

describe("CanonicalKernel native binding", () => {
  const fixture = JSON.parse(
    readFileSync(
      join(process.cwd(), "../tests/fixtures/kernel-wire/golden_lifecycle_agent_root.json"),
      "utf8",
    ),
  ) as {
    genesis_digest: string
    links: Array<{ envelope: unknown; record: unknown }>
  }

  it("passes through core-owned record bytes and digest", () => {
    const native = getKernel()
    expect(native.kernelAbiVersion()).toBe(3)
    const kernel = new native.CanonicalKernel()

    const prepared = kernel.prepare(JSON.stringify(fixture.links[0].envelope))
    expect(prepared.status).toBe("prepared")
    if (prepared.status !== "prepared") throw new Error("expected prepared")
    expect(prepared.stepSeq).toBe("0")
    expect(prepared.recordDigest).toBe(fixture.genesis_digest)
    expect(Buffer.isBuffer(prepared.recordBytes)).toBe(true)
    expect(prepared.recordBytes.toString("utf8")).toBe(JSON.stringify(fixture.links[0].record))

    const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
    expect(committed.stepSeq).toBe("0")
    expect(committed.recordDigest).toBe(fixture.genesis_digest)
    expect(kernel.lifecycle()).toBe("configured")
    expect("step" in kernel).toBe(false)

    const replayed = kernel.prepare(JSON.stringify(fixture.links[0].envelope))
    expect(replayed.status).toBe("replayed")
    if (replayed.status !== "replayed") throw new Error("expected replayed")
    expect(replayed.recordDigest).toBe(fixture.genesis_digest)

    const rebuilt = new native.CanonicalKernel()
    const restoreCost = rebuilt.restore(undefined, [prepared.recordBytes])
    expect(restoreCost.recordsBeforeCheckpoint).toBe("1")
    expect(restoreCost.recordsAfterCheckpoint).toBe("0")
    expect(rebuilt.lifecycle()).toBe("configured")
  })

  it("returns strict structured rejection and keeps restore in place", () => {
    const native = getKernel()
    const kernel = new native.CanonicalKernel()
    const malformed = kernel.prepare("{")
    expect(malformed.status).toBe("rejected")
    if (malformed.status !== "rejected") throw new Error("expected rejected")
    expect(JSON.parse(malformed.faultJson)).toMatchObject({ code: "malformed_envelope" })

    const prepared = kernel.prepare(JSON.stringify(fixture.links[0].envelope))
    if (prepared.status !== "prepared") throw new Error("expected prepared")
    kernel.commit(prepared.prepareToken, prepared.recordDigest)
    const checkpoint = kernel.checkpointCandidate()
    const identity = kernel
    kernel.restore(checkpoint.checkpointBytes, [])
    expect(kernel).toBe(identity)
    expect(kernel.lifecycle()).toBe("configured")
  })
})
