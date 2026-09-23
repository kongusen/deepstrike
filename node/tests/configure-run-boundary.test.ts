import { governancePolicyToKernelEvent } from "../src/governance.js"
import { normalizeContextPolicy, contextPolicy } from "../src/runtime/context-policy.js"
import { buildConfigureRunPolicyConfig, kernelReliabilityToKernel } from "../src/runtime/runner.js"
import { assertNativeProfile, signalPolicyToKernel } from "../src/runtime/os-profile.js"

describe("configure_run policy adapters", () => {
  it("projects governance and context policies into kernel vocabulary", () => {
    expect(governancePolicyToKernelEvent({
      defaultAction: "deny",
      vetoes: ["shell"],
      rateLimits: [{ tool: "search", maxCalls: 2, windowMs: 1000 }],
      constraints: [{ kind: "required", tool: "search", path: "query" }],
    })).toEqual({
      kind: "load_governance_policy",
      default_action: "deny",
      rules: [],
      vetoed_tools: ["shell"],
      rate_limits: [{ tool: "search", max_calls: 2, window_ms: 1000 }],
      constraints: [{ kind: "required", tool: "search", path: "query" }],
    })

    expect(normalizeContextPolicy(contextPolicy({ preserveRecentTurns: 3 }))).toMatchObject({
      preserve_recent_turns: 3,
      pressure_thresholds_ppm: { snip: 700000, micro: 800000, collapse: 900000, auto: 950000, renewal: 980000 },
    })
  })

  it("projects reliability and signal policies with explicit defaults", () => {
    expect(kernelReliabilityToKernel({ providerRecoveryAttempts: 2, maxInputBytes: 4096 })).toEqual({
      provider_recovery_attempts: 2,
      max_input_bytes: 4096,
    })
    expect(signalPolicyToKernel({ queueMax: 8, ttlMs: 5000, deadlineEscalation: true })).toEqual({
      queue_max: 8,
      ttl_ms: 5000,
      deadline_escalation: true,
    })
  })

  it("correlates child policy adapters into one configure_run policy config", () => {
    const config = buildConfigureRunPolicyConfig({
      governancePolicy: { vetoes: ["shell"] },
      signalPolicy: { queueMax: 8 },
      contextPolicy: { preserveRecentTurns: 3 },
      kernelReliability: { maxInputBytes: 4096 },
    }, assertNativeProfile())

    expect(config).toMatchObject({
      governance: { vetoed_tools: ["shell"] },
      signal_policy: { queue_max: 8 },
      context_policy: { preserve_recent_turns: 3 },
      reliability: { max_input_bytes: 4096 },
    })
  })
})
