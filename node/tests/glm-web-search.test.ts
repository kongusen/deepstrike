/**
 * GLM web_search — vendor server tool injected into tools[] via extensions (OpenAI-wire only).
 * Mechanism test (no network): the server tool is shaped correctly and the `web_search` key is stripped
 * from the request-body passthrough so it only shapes tools[].
 */
import { GLMProvider } from "../src/providers/glm.js"

// Reach the protected hooks for a white-box check (they are the seam vendors override).
type Hooks = {
  serverTools(ext?: Record<string, unknown>): unknown[]
  prepareExtensions(ext?: Record<string, unknown>): Record<string, unknown> | undefined
}

describe("GLM web_search server tool", () => {
  const p = new GLMProvider("k") as unknown as Hooks

  it("injects a web_search tool entry when extensions.web_search is truthy", () => {
    expect(p.serverTools({ web_search: true })).toEqual([{ type: "web_search", web_search: {} }])
  })

  it("passes a config object through to the web_search entry", () => {
    expect(p.serverTools({ web_search: { count: 5, search_recency_filter: "oneWeek" } })).toEqual([
      { type: "web_search", web_search: { count: 5, search_recency_filter: "oneWeek" } },
    ])
  })

  it("injects nothing when web_search is absent/falsy", () => {
    expect(p.serverTools({})).toEqual([])
    expect(p.serverTools({ web_search: false })).toEqual([])
    expect(p.serverTools(undefined)).toEqual([])
  })

  it("strips web_search from the request-body passthrough (shapes tools[] only)", () => {
    expect(p.prepareExtensions({ web_search: true, temperature: 0.2 })).toEqual({ temperature: 0.2 })
    expect(p.prepareExtensions({ temperature: 0.2 })).toEqual({ temperature: 0.2 })
  })
})
