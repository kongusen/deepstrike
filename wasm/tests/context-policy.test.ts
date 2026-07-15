import { contextPolicyV1, kernelRecordDigest, normalizeContextPolicyV1, ratioToPpm } from "../src/runtime/index.js"

describe("WASM ContextPolicyV1", () => {
  it("uses the cross-SDK integer ppm wire", async () => {
    const wire = normalizeContextPolicyV1(contextPolicyV1())
    expect(wire.pressure_thresholds_ppm).toEqual({
      snip: 700_000,
      micro: 800_000,
      collapse: 900_000,
      auto: 950_000,
      renewal: 980_000,
    })
    expect(wire.target_after_compress_ppm).toBe(650_000)
    await expect(kernelRecordDigest(wire)).resolves.toBe(
      "a8ea8875b056cb07c15b7832b5a90aa809041e91aeaf58462c402bce2312351b",
    )
    expect(ratioToPpm(0.1234565)).toBe(123_457)
  })
})
