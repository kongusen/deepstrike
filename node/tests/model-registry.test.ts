import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { createProvider, resolveProviderRuntime } from "../src/providers/catalog.js"
import { deepseek, glm, kimi, minimax, qwen } from "../src/providers/factories.js"
import {
  MODEL_CAPABILITY_STATES,
  cacheCapabilityEvidence,
  getRuntimePolicy,
  modelRegistry,
  registryEvidence,
  resolveEffectiveCapability,
} from "../src/providers/model-registry.js"

describe("SPC-013 A-01 model registry", () => {
  it("contains no retired model-table API", () => {
    const here = path.dirname(fileURLToPath(import.meta.url))
    const sources = fs.readdirSync(path.join(here, "../src/providers"))
      .filter(file => file.endsWith(".ts"))
      .map(file => fs.readFileSync(path.join(here, "../src/providers", file), "utf8"))
      .join("\n")
    expect(sources).not.toMatch(/\bmodelProfiles\b|\bgetModelProfile\b|\bModelProfileId\b/)
  })

  it("records evidence for every non-intrinsic registry rule", () => {
    expect(new Set(registryEvidence.map(entry => entry.classification))).toEqual(
      new Set(["routing", "policy", "protocol", "endpoint"]),
    )
    for (const evidence of registryEvidence) {
      expect(evidence.source).not.toBe("")
      expect(evidence.verifiedAt).toBe("2026-08-12")
    }
  })

  it("resolves generation and embedding dynamically with intrinsic facts unknown", () => {
    expect(modelRegistry.resolve("openai/gpt-5.5")?.descriptor).toEqual({
      id: "openai/gpt-5.5",
      providerId: "openai",
      kind: "generation",
      intrinsic: {},
    })
    expect(modelRegistry.resolve("openai/text-embedding-4-future")?.descriptor.kind).toBe("embedding")
    expect(modelRegistry.resolve("openai/future-custom-model")?.descriptor.intrinsic).toEqual({})
  })

  it.each([
    ["openai/gpt-4o", "openai.chat"],
    ["openai/gpt-5.5", "openai.responses"],
    ["openai/o3-mini", "openai.responses"],
    ["openai/text-embedding-3-large", "openai.embeddings"],
    ["qwen/text-embedding-v4", "qwen.dashscope.embeddings"],
    ["qwen/qwen3-vl-embedding", "qwen.dashscope.multimodal-embeddings"],
    ["gemini/gemini-embedding-2", "gemini.google.embeddings"],
    ["glm/embedding-3", "glm.openai.embeddings"],
  ] as const)("preserves evidenced routing for %s", (modelId, endpointId) => {
    expect(modelRegistry.resolve(modelId)?.defaultEndpointId).toBe(endpointId)
  })

  it.each([
    [["supported"], "supported", ["model"]],
    [["supported", "supported"], "supported", ["model", "protocol"]],
    [["supported", "unknown"], "unknown", ["model"]],
    [["unknown", "unknown"], "unknown", []],
    [["supported", "unsupported", "unknown"], "unsupported", ["model", "protocol"]],
  ] as const)("resolves tri-state layers %j", (states, expected, evidence) => {
    const layers = ["model", "protocol", "endpoint"] as const
    const resolved = resolveEffectiveCapability(states.map((state, index) => ({
      layer: layers[index],
      state,
    })))
    expect(resolved.state).toBe(expected)
    expect(resolved.evidence).toEqual(evidence)
    expect(MODEL_CAPABILITY_STATES).toContain(resolved.state)
  })

  it("downgrades endpoint-only capability for an unverified custom baseURL", () => {
    const custom = resolveProviderRuntime({
      model: "anthropic/claude-sonnet-4-6",
      apiKey: "k",
      baseURL: "https://proxy.invalid",
    })
    expect(custom.effectiveCapabilities.nativeTokenCounting.state).toBe("unknown")

    const explicit = resolveProviderRuntime({
      model: "anthropic/claude-sonnet-4-6",
      apiKey: "k",
      endpoint: "anthropic.messages",
      baseURL: "https://proxy.invalid",
    })
    expect(explicit.effectiveCapabilities.nativeTokenCounting.state).toBe("supported")
  })

  it("never reports native token counting without an adapter method", () => {
    for (const model of ["openai/gpt-5.5", "anthropic/claude-sonnet-4-6", "gemini/gemini-2.5-pro"]) {
      const runtime = resolveProviderRuntime({ model, apiKey: "k" })
      if (runtime.effectiveCapabilities.nativeTokenCounting.state === "supported") {
        expect(typeof runtime.adapter.countTokens).toBe("function")
      }
    }
  })

  it("owns Ollama prefix policy and GLM alias normalization in one resolver", () => {
    expect(modelRegistry.resolve("llama3.1-70b", "ollama")?.recommendedRuntimePolicy).toEqual({ maxTurns: 20 })
    expect(modelRegistry.resolve("totally-unknown-xyz", "ollama")?.descriptor).toEqual({
      id: "ollama/totally-unknown-xyz",
      providerId: "ollama",
      kind: "generation",
      intrinsic: {},
    })
    expect(getRuntimePolicy("glm", "glm-5.2")).toEqual({ maxTurns: 50 })
    expect(getRuntimePolicy("glm", "glm/glm-5.2")).toEqual({ maxTurns: 50 })
  })

  it("keeps protocol adapters independent from the registry", () => {
    const here = path.dirname(fileURLToPath(import.meta.url))
    for (const file of [
      "anthropic-adapter.ts", "gemini-adapter.ts", "ollama-adapter.ts",
      "openai-chat.ts", "openai-responses-adapter.ts", "protocol-adapter.ts",
    ]) {
      const source = fs.readFileSync(path.join(here, "../src/providers", file), "utf8")
      expect(source).not.toMatch(/from ["']\.\/model-registry\.js["']|modelRegistry|resolveProviderRuntime|getRuntimePolicy|getModelProfile/)
    }
  })

  it("injects policy without provider classes querying the registry", () => {
    const here = path.dirname(fileURLToPath(import.meta.url))
    for (const file of [
      "anthropic.ts", "anthropic-compatible.ts", "openai.ts", "openai-responses.ts",
      "gemini.ts", "ollama.ts",
    ]) {
      const source = fs.readFileSync(path.join(here, "../src/providers", file), "utf8")
      expect(source).not.toMatch(/from ["']\.\/model-registry/)
    }
    expect(resolveProviderRuntime({ model: "openai/gpt-5.5", apiKey: "k" }).adapter.runtimePolicy?.())
      .toEqual({ maxTurns: 60 })
  })
})

describe("SPC-020 provider default routing", () => {
  const defaults = [
    ["deepseek", "deepseek.openai", "openai-chat"],
    ["kimi", "kimi.openai", "openai-chat"],
    ["qwen", "qwen.dashscope", "openai-chat"],
    ["glm", "glm.openai", "openai-chat"],
    ["minimax", "minimax.anthropic", "anthropic-messages"],
  ] as const

  it.each(defaults)("routes %s consistently through the model and runtime registries", (provider, endpoint, protocol) => {
    expect(modelRegistry.resolve(`${provider}/fixture-model`)?.defaultEndpointId).toBe(endpoint)
    expect(resolveProviderRuntime({ model: `${provider}/fixture-model`, apiKey: "k" }).identity)
      .toMatchObject({ providerId: provider, endpointId: endpoint, protocol })
    expect(createProvider({ model: `${provider}/fixture-model`, apiKey: "k" }).descriptor?.()?.protocol)
      .toBe(protocol)
  })

  it.each([
    ["deepseek", deepseek, "openai-chat"],
    ["kimi", kimi, "openai-chat"],
    ["qwen", qwen, "openai-chat"],
    ["glm", glm, "openai-chat"],
    ["minimax", minimax, "anthropic-messages"],
  ] as const)("keeps the %s public factory on the same default protocol", (_provider, factory, protocol) => {
    expect(factory({ apiKey: "k", model: "fixture-model" }).descriptor?.()?.protocol).toBe(protocol)
  })

  it("keeps explicit compatible endpoints available", () => {
    expect(resolveProviderRuntime({
      model: "deepseek/fixture-model",
      apiKey: "k",
      endpoint: "deepseek.anthropic",
    }).identity.protocol).toBe("anthropic-messages")
    expect(resolveProviderRuntime({
      model: "minimax/fixture-model",
      apiKey: "k",
      endpoint: "minimax.openai",
    }).identity.protocol).toBe("openai-chat")
  })
})

describe("SPC-021 endpoint cache evidence", () => {
  it("backs every supported cache claim with endpoint evidence", () => {
    expect(cacheCapabilityEvidence.map(entry => entry.endpointId).sort()).toEqual([
      "anthropic.messages",
      "deepseek.openai",
    ])
    for (const entry of cacheCapabilityEvidence) {
      expect(entry.source).toMatch(/^https:\/\//)
      expect(entry.verifiedAt).toBe("2026-08-26")
      expect(entry.classification).toBe("documentation")
      expect(entry.usageFields.length).toBeGreaterThan(0)
    }
  })

  it.each([
    ["anthropic/claude-sonnet-4-6", undefined, "supported"],
    ["deepseek/deepseek-chat", undefined, "supported"],
    ["deepseek/deepseek-chat", "deepseek.anthropic", "unknown"],
    ["kimi/kimi-k2.6", undefined, "unknown"],
    ["qwen/qwen3.6-plus", undefined, "unknown"],
    ["glm/glm-5.2", undefined, "unknown"],
    ["minimax/MiniMax-M3", undefined, "unknown"],
  ] as const)("resolves cache evidence for %s on %s", (model, endpoint, expected) => {
    expect(resolveProviderRuntime({ model, apiKey: "k", ...(endpoint ? { endpoint } : {}) })
      .effectiveCapabilities.promptCaching.state).toBe(expected)
  })

  it("does not carry built-in cache evidence onto a custom base URL", () => {
    expect(resolveProviderRuntime({
      model: "deepseek/deepseek-chat",
      apiKey: "k",
      baseURL: "https://proxy.invalid/v1",
    }).effectiveCapabilities.promptCaching.state).toBe("unknown")
  })
})
