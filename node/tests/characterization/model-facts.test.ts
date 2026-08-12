/**
 * SPC-013 A-00 historical evidence. The retired modelProfiles implementation is
 * intentionally not imported: A-01 replaced that whitelist-shaped design with
 * dynamic registry resolvers. This test only protects the frozen migration input.
 */
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { endpointProfiles } from "../../src/providers/endpoints.js"
import { getRuntimePolicy } from "../../src/providers/model-registry.js"
import { expectGolden } from "./golden.js"

describe("spc_013-A-00 characterization: retired model table", () => {
  it("keeps all 96 retired records as immutable evidence", () => {
    const here = path.dirname(fileURLToPath(import.meta.url))
    const rows = JSON.parse(fs.readFileSync(
      path.join(here, "__golden__/model-facts-baseline.json"),
      "utf8",
    )) as Array<{ id: string }>
    expect(rows).toHaveLength(96)
    expect(new Set(rows.map(row => row.id)).size).toBe(96)
  })

  it("locks the endpoint profile table", () => {
    const { "ollama.local": _newEndpoint, ...historicalEndpoints } = endpointProfiles
    expectGolden("endpoint-profiles", historicalEndpoints)
  })

  it("locks the Ollama prefix policy resolver", () => {
    const cases = ["llama3", "llama3.2", "llama3.1-70b", "qwen2.5", "mistral-nemo", "totally-unknown-xyz"]
    expectGolden("ollama-prefix-policies",
      cases.map(model => ({ model, policy: getRuntimePolicy("ollama", model) })),
    )
  })

  it("locks GLM bare-name/prefixed-name normalization", () => {
    expectGolden("glm-policy-dual-keys", {
      bare: getRuntimePolicy("glm", "glm-5.2"),
      prefixed: getRuntimePolicy("glm", "glm/glm-5.2"),
      unknownModel: getRuntimePolicy("glm", "glm-no-such"),
    })
  })
})
