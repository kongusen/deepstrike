// ─── Declarative policy (in-kernel gate) ──────────────────────────────────────

type GovernancePolicyAction = "allow" | "deny" | "ask_user"

export interface GovernancePolicy {
  defaultAction?: GovernancePolicyAction
  rules?: { pattern: string; action: GovernancePolicyAction }[]
  vetoes?: string[]
  rateLimits?: { tool: string; maxCalls: number; windowMs: number }[]
  constraints?: GovernanceConstraint[]
  /** I5: when true (default), the kernel withholds statically denied tools (vetoes and `deny`
   *  rules, evaluated exactly as the call gate evaluates them) from the provider surface and names
   *  them once in the knowledge slot, so the model does not plan around them. Set to false when the
   *  agent should learn the denial through a real attempted call and its visible error result. */
  surfaceDeniedInSystem?: boolean
}

export type GovernanceConstraint =
  | { kind: "required"; tool: string; path: string }
  | { kind: "enum"; tool: string; path: string; values: string[] }
  | { kind: "range"; tool: string; path: string; min?: number; max?: number }

export interface KernelGovernancePolicy {
  [key: string]: unknown
  kind: "load_governance_policy"
  default_action?: GovernancePolicyAction
  rules: Array<{ tool_pattern: string; action: GovernancePolicyAction }>
  vetoed_tools: string[]
  rate_limits: Array<{ tool: string; max_calls: number; window_ms: number }>
  constraints: Array<Record<string, unknown>>
  hide_denied_tools: boolean
}

/**
 * Convert a declarative {@link GovernancePolicy} into the `load_governance_policy`
 * kernel event payload (snake_case wire fields). Pure — no side effects.
 */
export function governancePolicyToKernelEvent(policy: GovernancePolicy): KernelGovernancePolicy {
  return {
    kind: "load_governance_policy",
    ...(policy.defaultAction ? { default_action: policy.defaultAction } : {}),
    rules: (policy.rules ?? []).map(r => ({ tool_pattern: r.pattern, action: r.action })),
    vetoed_tools: policy.vetoes ?? [],
    rate_limits: (policy.rateLimits ?? []).map(rl => ({
      tool: rl.tool,
      max_calls: rl.maxCalls,
      window_ms: rl.windowMs,
    })),
    constraints: (policy.constraints ?? []).map(c =>
      c.kind === "enum"
        ? { kind: "enum", tool: c.tool, param_path: c.path, values: c.values }
        : c.kind === "range"
          ? {
              kind: "range", tool: c.tool, param_path: c.path,
              // The wire carries fixed-point micro-units so a bound replays byte-identically.
              ...(c.min !== undefined ? { min_micros: toMicros(c.min) } : {}),
              ...(c.max !== undefined ? { max_micros: toMicros(c.max) } : {}),
            }
          : { kind: "required", tool: c.tool, param_path: c.path },
    ),
    hide_denied_tools: policy.surfaceDeniedInSystem !== false,
  }
}

/** A §13.2 live-policy patch replacing the governance posture, for `RuntimeRunner.applyPolicyPatch`. */
export function governancePolicyPatch(policy: GovernancePolicy): {
  kind: "replace_governance_policy"
  policy: Record<string, unknown>
} {
  const { kind: _kind, ...wire } = governancePolicyToKernelEvent(policy)
  return {
    kind: "replace_governance_policy",
    policy: {
      ...wire,
      rate_limits: wire.rate_limits.map(limit => ({ ...limit, window_ms: String(limit.window_ms) })),
    },
  }
}

function toMicros(value: number): number {
  if (!Number.isFinite(value)) throw new RangeError(`governance range bound must be finite, got ${value}`)
  return Math.round(value * 1_000_000)
}

/** A §13.2 live policy patch in kernel vocabulary — the closed set of policies that may change
 *  while an operation runs. Use `governancePolicyPatch` to build the governance variant. */
export type LivePolicyPatch =
  | { kind: "replace_signal_policy"; policy: Record<string, unknown> }
  | { kind: "replace_governance_policy"; policy: Record<string, unknown> }
  | { kind: "replace_recovery_policy"; policy: Record<string, unknown> }
  | {
      kind: "tighten_resource_quota"
      max_concurrent_subagents?: number
      max_total_subagents?: number
      max_spawn_depth?: number
      max_workflow_nodes?: number
    }
