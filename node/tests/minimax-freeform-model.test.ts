/**
 * Free-form vendor model names: the model registry is metadata + bare-name routing convenience,
 * NOT a runtime whitelist. Any vendor model id works through the vendor factory / a provider-qualified
 * createProvider / the vendor `prefix/model` form — registered or not. Registry entries only add a
 * default, recommended policy, and metadata.
 */
import { minimax } from "../src/providers/factories.js"
import { createProvider } from "../src/providers/catalog.js"
import { MiniMaxAnthropicProvider } from "../src/providers/minimax.js"

const FUTURE = "MiniMax-M99-does-not-exist-in-registry"

describe("free-form vendor model names (not a whitelist)", () => {
  it("vendor factory passes an UNREGISTERED model straight through", () => {
    const p = minimax({ apiKey: "k", model: FUTURE })
    expect(p.descriptor().model).toBe(FUTURE) // wire model = exactly what you passed
  })

  it("direct provider constructor accepts any model", () => {
    const p = new MiniMaxAnthropicProvider("k", FUTURE)
    expect(p.descriptor().model).toBe(FUTURE)
  })

  it("createProvider routes an unregistered model when the provider is given", () => {
    const p = createProvider({ model: FUTURE, provider: "minimax", apiKey: "k" })
    expect(p.descriptor().model).toBe(FUTURE)
  })

  it("createProvider routes an unregistered model via the `vendor/model` prefix", () => {
    const p = createProvider({ model: `minimax/${FUTURE}`, apiKey: "k" })
    expect(p.descriptor().model).toBe(FUTURE) // prefix is stripped for the wire
  })

  it("only a BARE unknown model with no provider hint is rejected (routing ambiguity), with a guiding message", () => {
    expect(() => createProvider({ model: FUTURE, apiKey: "k" })).toThrow(/Pass provider or endpoint for custom model names/)
  })
})
