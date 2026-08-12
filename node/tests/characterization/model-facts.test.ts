/**
 * spc_013-A-00: model-facts migration baseline.
 *
 * Dumps, for EVERY static model profile, the effective view today's code produces:
 * the `modelProfiles` record itself, the `ModelCapabilities` projection
 * (spc_011-D), and the runtime policy resolved through the REAL production dispatch
 * (`createProvider`, which picks the endpoint + class exactly as production does).
 * Card 013-A-01 (ModelRegistry 收敛) must reproduce this file field-for-field.
 *
 * Also locks two irregular resolution paths that a registry收敛 must not "tidy away":
 * Ollama's prefix-match policy resolver (models are not in the static catalog) and GLM's
 * bare-name/prefixed-name dual keys (vendor-profiles.ts:61-73).
 */
import { createProvider } from "../../src/providers/catalog.js"
import { tryGetModelCapabilities } from "../../src/providers/model-capabilities.js"
import { endpointProfiles, getModelProfile, modelProfiles } from "../../src/providers/profiles.js"
import { OllamaProvider } from "../../src/providers/ollama.js"
import { GLMProvider } from "../../src/providers/glm.js"
import { expectGolden } from "./golden.js"

describe("spc_013-A-00 characterization: model facts baseline", () => {
  it("locks the effective facts of every static model profile", () => {
    const ids = Object.keys(modelProfiles).sort()
    const baseline = ids.map(id => {
      const profile = getModelProfile(id)
      const modelName = id.slice(profile.providerId.length + 1)
      let providerClass: string | null = null
      let runtimePolicy: unknown = null
      let dispatchError: string | null = null
      try {
        const provider = createProvider({ model: id, apiKey: "characterization-fake-key" })
        providerClass = provider.constructor.name
        runtimePolicy = provider.runtimePolicy()
      } catch (err) {
        // Embedding endpoints etc. have no generation factory — that IS today's behavior.
        dispatchError = err instanceof Error ? err.message : String(err)
      }
      return {
        id,
        profile,
        capabilities: tryGetModelCapabilities(profile.providerId, modelName) ?? null,
        providerClass,
        runtimePolicy,
        dispatchError,
      }
    })
    expectGolden("model-facts-baseline", baseline)
  })

  it("locks the endpoint profile table (registry input for A-06)", () => {
    expectGolden("endpoint-profiles", endpointProfiles)
  })

  it("locks the Ollama prefix policy resolver", () => {
    const cases = ["llama3", "llama3.2", "llama3.1-70b", "qwen2.5", "mistral-nemo", "totally-unknown-xyz"]
    expectGolden("ollama-prefix-policies",
      cases.map(model => ({ model, policy: new OllamaProvider(model).runtimePolicy() })),
    )
  })

  it("locks GLM bare-name/prefixed-name dual-key behavior", () => {
    expectGolden("glm-policy-dual-keys", {
      bare: new GLMProvider("k", "glm-5.2").runtimePolicy(),
      prefixed: new GLMProvider("k", "glm/glm-5.2").runtimePolicy(),
      unknownModel: new GLMProvider("k", "glm-no-such").runtimePolicy(),
    })
  })
})
