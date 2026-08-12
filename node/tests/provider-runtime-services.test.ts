import { CapabilityRouter } from "../src/providers/capability-router.js"
import {
  CredentialResolutionError,
  resolveCredential,
  type CredentialResolver,
} from "../src/providers/credentials.js"
import {
  DynamicModelCatalog,
  StaticModelCatalog,
  type ModelCatalogSource,
  type ModelRegistration,
} from "../src/providers/model-catalog.js"
import { resolveProviderRuntime, resolveProviderRuntimeAsync } from "../src/providers/catalog.js"

function registration(
  id: string,
  intrinsic: ModelRegistration["descriptor"]["intrinsic"] = {},
  contextWindow?: number,
): ModelRegistration {
  return {
    descriptor: {
      id,
      providerId: "openai",
      kind: "generation",
      ...(contextWindow === undefined ? {} : { contextWindow }),
      intrinsic,
    },
    defaultEndpointId: "openai.chat",
  }
}

describe("spc_015-03 credential resolver", () => {
  it("fails closed when a credential-required provider has no configured source", () => {
    expect(() => resolveProviderRuntime({ model: "openai/gpt-4o" }))
      .toThrow(CredentialResolutionError)
    expect(() => resolveProviderRuntime({ model: "openai/gpt-4o" }))
      .toThrow("Missing credential")
  })

  it("resolves API keys, bearer credentials, and injected custom resolvers", async () => {
    await expect(resolveCredential({
      providerId: "openai",
      modelId: "gpt-4o",
      endpointId: "openai.chat",
      protocol: "openai-chat",
    }, { apiKey: "api-key" })).resolves.toEqual({ type: "api_key", value: "api-key" })

    await expect(resolveCredential({
      providerId: "anthropic",
      modelId: "claude-sonnet-4-6",
      endpointId: "anthropic.messages",
      protocol: "anthropic-messages",
    }, { bearerToken: "bearer-token" })).resolves.toEqual({ type: "bearer", value: "bearer-token" })

    const resolver: CredentialResolver = request => request.providerId === "openai"
      ? { type: "api_key", value: "from-resolver" }
      : undefined
    await expect(resolveCredential({
      providerId: "openai",
      modelId: "gpt-4o",
      endpointId: "openai.chat",
      protocol: "openai-chat",
    }, { credentialResolver: resolver })).resolves.toEqual({ type: "api_key", value: "from-resolver" })
  })

  it("never places secret values into credential errors or their JSON projection", async () => {
    const secret = "top-secret-value"
    const resolver: CredentialResolver = () => ({ type: "api_key", value: "   " })
    await expect(resolveCredential({
      providerId: "openai",
      modelId: "gpt-4o",
      endpointId: "openai.chat",
      protocol: "openai-chat",
    }, { credentialResolver: resolver })).rejects.toThrow(CredentialResolutionError)

    try {
      await resolveCredential({
        providerId: "openai",
        modelId: "gpt-4o",
        endpointId: "openai.chat",
        protocol: "openai-chat",
      }, { apiKey: secret, bearerToken: "other-secret" })
      throw new Error("expected credential resolution to fail")
    } catch (error) {
      expect(String(error)).not.toContain(secret)
      expect(JSON.stringify(error)).not.toContain(secret)
    }
  })

  it("binds bearer authentication for Anthropic without exposing it in the resolved runtime identity", () => {
    const secret = "bearer-secret"
    const runtime = resolveProviderRuntime({
      model: "anthropic/claude-sonnet-4-6",
      bearerToken: secret,
    })

    expect((runtime.adapter as unknown as { client: { authToken: string | null; apiKey: string | null } }).client)
      .toMatchObject({ authToken: secret, apiKey: null })
    expect(JSON.stringify(runtime.identity)).not.toContain(secret)
  })
})

describe("spc_015-04 static and dynamic model catalogs", () => {
  it("keeps static list/get deterministic and independent from dynamic discovery", async () => {
    const staticCatalog = new StaticModelCatalog([
      registration("openai/static-a"),
      registration("openai/static-b"),
    ])

    await expect(staticCatalog.list()).resolves.toEqual([
      registration("openai/static-a"),
      registration("openai/static-b"),
    ])
    await expect(staticCatalog.get("openai/static-b")).resolves.toEqual(registration("openai/static-b"))
    await expect(staticCatalog.get("openai/missing")).resolves.toBeUndefined()
  })

  it("preserves static fallback and the last successful dynamic snapshot when refresh fails", async () => {
    let fail = false
    const source: ModelCatalogSource = {
      async list() {
        if (fail) throw new Error("remote catalog unavailable")
        return [registration("openai/dynamic")]
      },
    }
    const catalog = new DynamicModelCatalog(source, new StaticModelCatalog([registration("openai/static")]))

    await expect(catalog.refresh()).resolves.toEqual({ ok: true })
    fail = true
    await expect(catalog.refresh()).resolves.toEqual({ ok: false, errorCode: "refresh_failed" })
    await expect(catalog.get("openai/dynamic")).resolves.toEqual(registration("openai/dynamic"))
    await expect(catalog.get("openai/static")).resolves.toEqual(registration("openai/static"))
  })

  it("uses an injected catalog during async provider runtime resolution", async () => {
    const catalog = new StaticModelCatalog([registration("openai/private", { tools: true })])
    const runtime = await resolveProviderRuntimeAsync({
      model: "openai/private",
      apiKey: "key",
      modelCatalog: catalog,
    })
    expect(runtime.model).toEqual(registration("openai/private", { tools: true }).descriptor)
  })
})

describe("spc_015-05 capability router", () => {
  const catalog = new StaticModelCatalog([
    registration("openai/text", { inputModalities: ["text"], tools: false, reasoning: false }, 8_000),
    registration("openai/vision", { inputModalities: ["text", "image"], tools: true, reasoning: true }, 128_000),
    registration("openai/unknown"),
  ])

  const candidates = ["openai/text", "openai/vision", "openai/unknown"].map(model => ({
    model,
    apiKey: "key",
    modelCatalog: catalog,
  }))

  it("filters known unsupported image, tool, reasoning, and context candidates", async () => {
    const result = await new CapabilityRouter().route({
      requiredInputModalities: ["image"],
      tools: true,
      reasoning: true,
      minimumContextWindow: 64_000,
    }, candidates)

    expect(result).toMatchObject({ ok: true, runtime: { identity: { modelId: "vision" } } })
    if (result.ok) {
      expect((result.runtime.adapter as unknown as { resolvedRuntime: unknown }).resolvedRuntime).toBe(result.runtime)
    }
  })

  it("keeps unknown capability evidence eligible for forward-compatible routing", async () => {
    const result = await new CapabilityRouter().route({ tools: true, reasoning: true }, [candidates[2]])
    expect(result).toMatchObject({ ok: true, runtime: { identity: { modelId: "unknown" } } })
  })

  it("returns a structured no-candidate result without credentials or transport diagnostics", async () => {
    const result = await new CapabilityRouter().route({
      requiredInputModalities: ["audio"],
      minimumContextWindow: 1_000_000,
    }, [candidates[0]])
    expect(result).toEqual({
      ok: false,
      error: {
        code: "no_capable_model",
        requirement: { requiredInputModalities: ["audio"], minimumContextWindow: 1_000_000 },
        candidates: [{ model: "openai/text", rejectedBy: ["input:audio", "context"] }],
      },
    })
  })
})
