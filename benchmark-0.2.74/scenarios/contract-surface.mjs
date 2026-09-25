import { assertSdkVersion } from "../core/sdk.mjs"
import { assertPublicContracts, PUBLIC_CONTRACTS } from "../contracts/manifest.mjs"
import { count } from "../core/metrics.mjs"

export const contractSurface = {
  id: "contract-surface",
  description: "0.2.74 public barrel and root leakage contract",
  variants: ["default"],
  surfaces: PUBLIC_CONTRACTS.map(contract => contract.surface),
  async run({ sdk }) {
    assertSdkVersion(sdk)
    const report = assertPublicContracts(sdk)
    return {
      metrics: {
        contractsPassed: count(report.results.filter(result => result.passed).length),
        contractsTotal: count(report.results.length),
        surfaces: count(new Set(report.results.map(result => result.surface)).size),
      },
      evidence: { contracts: report.results.map(result => ({ id: result.id, surface: result.surface, passed: result.passed })) },
    }
  },
}
