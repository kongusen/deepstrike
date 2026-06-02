import { LargeResultSpool, SpoolStorageDriver } from '../src/runtime/large-result-spool.js'

describe("WASM LargeResultSpool", () => {
  it("does not spool small outputs under the threshold", async () => {
    const spool = new LargeResultSpool({
      spoolThresholdBytes: 100,
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

  it("spools large outputs above the threshold and provides preview in memory", async () => {
    const spool = new LargeResultSpool({
      spoolThresholdBytes: 10,
      previewTokens: 5,
    })

    const largeResult = {
      callId: "call-2",
      tool: "some-tool",
      output: "this is a very long output content",
    }

    const processed = await spool.processToolResult(largeResult)
    expect(processed.wasSpooled).toBe(true)
    expect(processed.spoolRef).toContain(".spool/")
    expect(processed.preview).toContain("[tool_result_spooled]")
    expect(processed.preview).toContain("size: 34 bytes")

    // Read back and verify content is identical
    const content = await spool.readSpooledResult(processed.spoolRef)
    expect(content).toBe("this is a very long output content")
  })

  it("supports a custom spool storage driver (e.g. KV or LocalStorage mock)", async () => {
    const customStore = new Map<string, string>()
    const driver: SpoolStorageDriver = {
      write: (key, val) => { customStore.set(key, val) },
      read: (key) => customStore.get(key) || "",
      delete: (key) => { customStore.delete(key) },
      list: () => Array.from(customStore.keys()),
    }

    const spool = new LargeResultSpool({
      spoolThresholdBytes: 10,
      driver,
    })

    const result = {
      callId: "call-custom",
      tool: "custom-tool",
      output: "large data that needs storing",
    }

    const processed = await spool.processToolResult(result)
    expect(processed.wasSpooled).toBe(true)
    expect(customStore.has(processed.spoolRef)).toBe(true)
    expect(customStore.get(processed.spoolRef)).toBe("large data that needs storing")
  })
})
