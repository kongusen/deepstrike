import { createProviderRequestPlan, measurementForPlan, recordPromptMeasurement } from "../src/providers/request-plan.js"
import { readFileSync } from "node:fs"
import { join } from "node:path"

describe("spc_016-01 WASM request plan", () => {
  it("uses the shared cross-SDK SHA-256 fingerprint fixture", () => {
    const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/provider-request-plan/v1.json"), "utf8")) as {
      input: Parameters<typeof createProviderRequestPlan>[0]
      fingerprint: string
    }
    const plan = createProviderRequestPlan(fixture.input)
    expect(plan.fingerprint).toBe(fixture.fingerprint)
    expect(plan.options).toEqual({ temperature: 0.2, auth: { mode: "request" }, transport: {} })
  })

  it("excludes secret and retry-only options while binding measurements to the exact fingerprint", () => {
    const args = { providerId: "openai", modelId: "gpt", endpoint: { id: "openai.chat", protocol: "openai-chat", baseURL: "https://api.openai.com/v1" }, context: { systemText: "s", turns: [{ role: "user" as const, content: "你好" }] }, tools: [] }
    const first = createProviderRequestPlan({ ...args, options: { apiKey: "secret", retry: { max: 1 }, temperature: 0.2 } })
    const same = createProviderRequestPlan({ ...args, options: { apiKey: "other", retry: { max: 99 }, temperature: 0.2 } })
    const changed = createProviderRequestPlan({ ...args, modelId: "gpt-next", options: { temperature: 0.2 } })
    const measurement = recordPromptMeasurement(first, { inputTokens: 14, source: { kind: "heuristic" }, confidence: "low_confidence" })
    expect(first.fingerprint).toBe(same.fingerprint)
    expect(first.fingerprint).not.toBe(changed.fingerprint)
    expect(JSON.stringify(first)).not.toContain("secret")
    expect(measurementForPlan(first, measurement)).toEqual(measurement)
    expect(measurementForPlan(changed, measurement)).toBeUndefined()
  })
})
