import { resolveProviderRuntime } from "../src/providers/catalog.js"

describe("SPC-013 effective modality semantics", () => {
  it("does not turn an unverified model claim into unsupported", () => {
    const resolved = resolveProviderRuntime({ model: "openai/gpt-4o", apiKey: "k" })
    expect(resolved.model.intrinsic.inputModalities).toBeUndefined()
    expect(resolved.effectiveCapabilities.inputModalities.audio.state).toBe("unknown")
  })

  it("keeps a protocol-level impossibility unsupported", () => {
    const resolved = resolveProviderRuntime({ model: "anthropic/future-model", apiKey: "k" })
    expect(resolved.effectiveCapabilities.inputModalities.audio.state).toBe("unsupported")
  })
})
