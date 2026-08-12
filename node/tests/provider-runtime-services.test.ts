import { CapabilityRouter } from "../src/providers/capability-router.js"
import {
  CredentialResolutionError,
  OAuthCredentialResolver,
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

  it("binds bearer authentication for OpenAI Responses without exposing it in the resolved runtime identity", () => {
    const secret = "responses-bearer-secret"
    const runtime = resolveProviderRuntime({
      model: "openai/gpt-4.1",
      endpoint: "openai.responses",
      bearerToken: secret,
    })

    expect((runtime.adapter as unknown as {
      client: { _options: { defaultHeaders?: Record<string, string> } }
    }).client._options.defaultHeaders).toEqual({ Authorization: `Bearer ${secret}` })
    expect(JSON.stringify(runtime.identity)).not.toContain(secret)
  })

  it("preserves explicit retry and custom endpoint inputs through provider construction", () => {
    const runtime = resolveProviderRuntime({
      model: "openai/gpt-4o",
      apiKey: "key",
      baseURL: "https://gateway.example.test/v1",
      retry: { maxRetries: 7, baseDelay: 13 },
    })
    const adapter = runtime.adapter as unknown as {
      client: { baseURL: string }
      maxRetries: number
      baseDelay: number
    }

    expect(adapter.client.baseURL).toBe("https://gateway.example.test/v1")
    expect(adapter.maxRetries).toBe(7)
    expect(adapter.baseDelay).toBe(13)
  })
})

describe("spc_016-03 OAuth credential extension", () => {
  const request = {
    providerId: "openai" as const,
    modelId: "gpt-4.1",
    endpointId: "openai.responses" as const,
    protocol: "openai-responses" as const,
  }

  it("deduplicates concurrent refreshes and refreshes again only after expiry", async () => {
    let now = 1_000
    let refreshes = 0
    let releaseRefresh: (() => void) | undefined
    const refreshStarted = new Promise<void>(resolve => { releaseRefresh = resolve })
    const resolver = new OAuthCredentialResolver({
      providerId: "openai",
      audience: "https://api.openai.com",
      requiredScopes: ["responses.write"],
      clock: () => now,
      refresh: async () => {
        refreshes += 1
        await refreshStarted
        return {
          accessToken: `access-${refreshes}`,
          expiresAt: now + 10,
          audience: "https://api.openai.com",
          scopes: ["responses.write"],
        }
      },
    })

    const pending = Array.from({ length: 3 }, () => resolveCredential(request, {
      credentialResolver: resolver.resolve,
    }))
    await Promise.resolve()
    expect(refreshes).toBe(1)
    releaseRefresh?.()
    await expect(Promise.all(pending)).resolves.toEqual([
      { type: "bearer", value: "access-1" },
      { type: "bearer", value: "access-1" },
      { type: "bearer", value: "access-1" },
    ])

    now += 11
    await expect(resolveCredential(request, { credentialResolver: resolver.resolve }))
      .resolves.toEqual({ type: "bearer", value: "access-2" })
    expect(refreshes).toBe(2)
  })

  it("fails closed on scope, audience, and revocation failures without exposing secrets", async () => {
    const secret = "oauth-refresh-secret"
    const wrongScope = new OAuthCredentialResolver({
      providerId: "openai",
      requiredScopes: ["responses.write"],
      refresh: async () => ({ accessToken: secret, expiresAt: Number.MAX_SAFE_INTEGER, scopes: ["read"] }),
    })
    await expect(resolveCredential(request, { credentialResolver: wrongScope.resolve }))
      .rejects.toMatchObject({ code: "credential_oauth_scope_mismatch", retryable: false })

    const wrongAudience = new OAuthCredentialResolver({
      providerId: "openai",
      audience: "https://api.openai.com",
      refresh: async () => ({ accessToken: secret, expiresAt: Number.MAX_SAFE_INTEGER, audience: "https://other.example" }),
    })
    await expect(resolveCredential(request, { credentialResolver: wrongAudience.resolve }))
      .rejects.toMatchObject({ code: "credential_oauth_audience_mismatch", retryable: false })

    const revoked = new OAuthCredentialResolver({
      providerId: "openai",
      refresh: async () => ({ accessToken: secret, expiresAt: Number.MAX_SAFE_INTEGER }),
    })
    revoked.revoke()
    await expect(resolveCredential(request, { credentialResolver: revoked.resolve }))
      .rejects.toMatchObject({ code: "credential_revoked", retryable: false })

    const refreshFailure = new OAuthCredentialResolver({
      providerId: "openai",
      refresh: async () => { throw new Error(secret) },
    })
    try {
      await resolveCredential(request, { credentialResolver: refreshFailure.resolve })
      throw new Error("expected OAuth refresh to fail")
    } catch (error) {
      expect(error).toMatchObject({ code: "credential_refresh_failed", retryable: true })
      expect(String(error)).not.toContain(secret)
      expect(JSON.stringify(error)).not.toContain(secret)
    }
  })

  it("constructs OpenAI Responses with only a refreshed bearer credential", async () => {
    const secret = "oauth-access-token"
    const resolver = new OAuthCredentialResolver({
      providerId: "openai",
      refresh: async () => ({ accessToken: secret, expiresAt: Number.MAX_SAFE_INTEGER }),
    })
    const runtime = await resolveProviderRuntimeAsync({
      model: "openai/gpt-4.1",
      endpoint: "openai.responses",
      credentialResolver: resolver.resolve,
    })

    expect((runtime.adapter as unknown as {
      client: { _options: { defaultHeaders?: Record<string, string> } }
    }).client._options.defaultHeaders).toEqual({ Authorization: `Bearer ${secret}` })
    expect(JSON.stringify(runtime.identity)).not.toContain(secret)
    const identity = (runtime.adapter as unknown as {
      requestPlanIdentity(): Record<string, unknown>
    }).requestPlanIdentity()
    expect(identity).toEqual({
      providerId: "openai",
      modelId: "gpt-4.1",
      endpoint: {
        id: "openai.responses",
        protocol: "openai-responses",
        baseURL: "https://api.openai.com/v1",
      },
    })
    expect(JSON.stringify(identity)).not.toContain(secret)
    expect(resolver.status()).toEqual({ providerId: "openai", revoked: false, hasUsableToken: true })
  })

  it("does not inherit OpenAI bearer policy on a compatible provider endpoint", async () => {
    const resolver = new OAuthCredentialResolver({
      providerId: "deepseek",
      refresh: async () => ({ accessToken: "deepseek-token", expiresAt: Number.MAX_SAFE_INTEGER }),
    })
    await expect(resolveProviderRuntimeAsync({
      model: "deepseek/deepseek-chat",
      credentialResolver: resolver.resolve,
    })).rejects.toMatchObject({ code: "credential_auth_mode_unsupported", retryable: false })
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
