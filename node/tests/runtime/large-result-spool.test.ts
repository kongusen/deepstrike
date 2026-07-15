import * as fs from 'fs/promises'
import * as path from 'path'
import { LargeResultSpool } from '../../src/runtime/large-result-spool.js'

describe("LargeResultSpool", () => {
  const testSpoolDir = path.join(process.cwd(), '.spool-test-dir')

  beforeEach(async () => {
    try {
      await fs.rm(testSpoolDir, { recursive: true, force: true })
    } catch {
      // Ignored
    }
  })

  afterAll(async () => {
    try {
      await fs.rm(testSpoolDir, { recursive: true, force: true })
    } catch {
      // Ignored
    }
  })

  it("does not spool small outputs under the threshold", async () => {
    const spool = new LargeResultSpool({
      spoolThresholdBytes: 100,
      spoolDir: testSpoolDir,
    })

    const smallResult = {
      callId: "call-1",
      tool: "some-tool",
      output: "small output",
    }

    const processed = await spool.processToolResult(smallResult)
    expect(processed.wasSpooled).toBe(false)
    expect(processed.originalOutput).toBe("small output")
    expect(processed.preview).toBe("small output")
    expect(processed.spoolRef).toBe("")
  })

  it("spools large outputs above the threshold and provides preview", async () => {
    const spool = new LargeResultSpool({
      spoolThresholdBytes: 10,
      spoolDir: testSpoolDir,
      previewTokens: 5,
    })

    const largeResult = {
      callId: "call-2",
      tool: "some-tool",
      output: "this is a very long output content",
    }

    const processed = await spool.processToolResult(largeResult)
    expect(processed.wasSpooled).toBe(true)
    expect(processed.spoolRef).toContain(testSpoolDir)
    expect(processed.preview).toContain("[tool_result_spooled]")
    expect(processed.preview).toContain("size: 34 bytes")

    // Read back and verify content is identical
    const content = await spool.readSpooledResult(processed.spoolRef)
    expect(content).toBe("this is a very long output content")
  })

  it("safeguards concurrent writes on the same target spool path", async () => {
    const spool = new LargeResultSpool({
      spoolThresholdBytes: 10,
      spoolDir: testSpoolDir,
    })

    const data = "duplicate-content-to-spool"

    // Trigger concurrent writes using persistOutput
    const p1 = spool.persistOutput("call-c", data)
    const p2 = spool.persistOutput("call-c", data)

    const [ref1, ref2] = await Promise.all([p1, p2])
    expect(ref1).toBe(ref2)

    const content = await spool.readSpooledResult(ref1)
    expect(content).toBe(data)
  })

  it("never derives a filesystem path from an untrusted call id", async () => {
    const spool = new LargeResultSpool({ spoolDir: testSpoolDir })
    const callId = "../../deepstrike-spool-escape"

    const ref = await spool.persistOutput(callId, "safe content")

    expect(path.dirname(ref)).toBe(testSpoolDir)
    expect(path.basename(ref)).not.toContain("..")
    expect(path.relative(testSpoolDir, ref)).not.toMatch(/^\.\./)
    expect(await spool.findByCallId(callId)).toBe("safe content")
  })

  it("performs TTL cleanup for expired spool files", async () => {
    const spool = new LargeResultSpool({
      spoolThresholdBytes: 10,
      spoolDir: testSpoolDir,
    })

    const result = {
      callId: "call-ttl",
      tool: "test-tool",
      output: "expired contents",
    }

    const processed = await spool.processToolResult(result)
    expect(processed.wasSpooled).toBe(true)

    // Verify it exists
    await expect(fs.access(processed.spoolRef)).resolves.toBeUndefined()

    // Run cleanup with maxAgeMs = -1 to force cleanup of any files
    const cleanedCount = await spool.cleanup(-1)
    expect(cleanedCount).toBe(1)

    // Verify it no longer exists
    await expect(fs.access(processed.spoolRef)).rejects.toThrow()
  })
})
