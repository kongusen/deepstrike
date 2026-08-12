import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { resolveProviderRuntime } from "../src/providers/catalog.js"
import {
  MODEL_CAPABILITY_STATES,
  getRuntimePolicy,
  modelRegistry,
  registryEvidence,
  resolveEffectiveCapability,
} from "../src/providers/model-registry.js"

describe("SPC-013 A-01 model registry", () => {
  it("keeps the retired 96-row table as historical evidence only", () => {
    const here = path.dirname(fileURLToPath(import.meta.url))
    const baseline = JSON.parse(fs.readFileSync(
      path.join(here, "characterization/__golden__/model-facts-baseline.json"),
      "utf8",
    )) as unknown[]
    expect(baseline).toHaveLength(96)

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
    for (const file of ["openai-chat.ts", "openai-responses.ts"]) {
      const source = fs.readFileSync(path.join(here, "../src/providers", file), "utf8")
      const adapterBody = source.match(/export class OpenAI(?:Chat|Responses)Adapter[\s\S]*?(?=\nexport class |$)/)?.[0]
      expect(adapterBody).toBeDefined()
      expect(adapterBody).not.toMatch(/modelRegistry|resolveProviderRuntime|getRuntimePolicy|getModelProfile/)
    }
  })

  it("injects policy without provider classes querying the registry", () => {
    const here = path.dirname(fileURLToPath(import.meta.url))
    for (const file of [
      "anthropic.ts", "anthropic-compatible.ts", "openai.ts", "openai-responses.ts",
      "gemini.ts", "ollama.ts", "deepseek.ts", "kimi.ts", "qwen.ts", "glm.ts", "minimax.ts",
    ]) {
      const source = fs.readFileSync(path.join(here, "../src/providers", file), "utf8")
      expect(source).not.toMatch(/from ["']\.\/model-registry/)
    }
    expect(resolveProviderRuntime({ model: "openai/gpt-5.5", apiKey: "k" }).adapter.runtimePolicy?.())
      .toEqual({ maxTurns: 60 })
  })
})
