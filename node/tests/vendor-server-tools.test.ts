/**
 * Vendor-native server tools / features exposed via `extensions`, mirrored Node↔Python. Each vendor
 * integrates at its own wire point — these are deterministic request-shaping checks (no network).
 * (GLM web_search has its own file: glm-web-search.test.ts.)
 */
import { QwenProvider } from "../src/providers/qwen.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import { GeminiProvider } from "../src/providers/gemini.js"

describe("Qwen enable_search (DashScope, via extra_body)", () => {
  const p = new QwenProvider("k") as unknown as {
    requestBodyExtras(e?: Record<string, unknown>): Record<string, unknown>
    requestExtensions(e?: Record<string, unknown>): Record<string, unknown>
  }
  it("puts enable_search (+ search_options) under extra_body", () => {
    expect(p.requestBodyExtras({ enable_search: true })).toEqual({ extra_body: { enable_search: true } })
    expect(p.requestBodyExtras({ enable_search: true, search_options: { forced_search: true } }))
      .toEqual({ extra_body: { enable_search: true, search_options: { forced_search: true } } })
  })
  it("does not emit extra_body when search is off", () => {
    expect(p.requestBodyExtras({})).toEqual({})
  })
  it("strips the search keys from the top-level passthrough", () => {
    const out = p.requestExtensions({ enable_search: true, search_options: { x: 1 }, temperature: 0.3 })
    expect(out).toEqual({ temperature: 0.3 })
  })
})

describe("OpenAI Responses built-in server tools", () => {
  const p = new OpenAIResponsesProvider("k") as unknown as {
    builtinTools(e?: Record<string, unknown>): Record<string, unknown>[]
    requestExtensions(e?: Record<string, unknown>): Record<string, unknown>
  }
  it("injects web_search (bool or config) + passes builtin_tools through", () => {
    expect(p.builtinTools({ web_search: true })).toEqual([{ type: "web_search" }])
    expect(p.builtinTools({ web_search: { search_context_size: "low" } }))
      .toEqual([{ type: "web_search", search_context_size: "low" }])
    expect(p.builtinTools({ builtin_tools: [{ type: "code_interpreter", container: { type: "auto" } }] }))
      .toEqual([{ type: "code_interpreter", container: { type: "auto" } }])
  })
  it("injects nothing by default", () => {
    expect(p.builtinTools({})).toEqual([])
  })
})

describe("Gemini google_search grounding + structured output", () => {
  const p = new GeminiProvider("k") as unknown as {
    vendorConfig(e?: Record<string, unknown>): { tools?: unknown[]; generationConfig?: Record<string, unknown> }
  }
  it("appends a googleSearch grounding tool", () => {
    expect(p.vendorConfig({ google_search: true })).toEqual({ tools: [{ googleSearch: {} }] })
    expect(p.vendorConfig({ google_search: { dynamic_threshold: 0.5 } }))
      .toEqual({ tools: [{ googleSearch: { dynamic_threshold: 0.5 } }] })
  })
  it("maps structured-output keys into generationConfig", () => {
    expect(p.vendorConfig({ response_mime_type: "application/json", response_schema: { type: "object" } }))
      .toEqual({ generationConfig: { responseMimeType: "application/json", responseSchema: { type: "object" } } })
  })
  it("returns empty config by default", () => {
    expect(p.vendorConfig({})).toEqual({})
  })
})
