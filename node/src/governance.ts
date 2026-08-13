// ─── Declarative policy (in-kernel gate) ──────────────────────────────────────

type GovernancePolicyAction = "allow" | "deny" | "ask_user"

export interface GovernancePolicy {
  defaultAction?: GovernancePolicyAction
  rules?: { pattern: string; action: GovernancePolicyAction }[]
  vetoes?: string[]
  rateLimits?: { tool: string; maxCalls: number; windowMs: number }[]
  constraints?: GovernanceConstraint[]
  /** I5: when true (default), the runner pre-filters denied tools out of the schema passed to the
   *  provider — the model never sees them and never tries to call them. The denied tool names are
   *  also surfaced as a single line on the system slot so the model knows not to plan around them.
   *  Set to false when the agent should learn the denial through a real attempted call and its
   *  visible error tool result. */
  surfaceDeniedInSystem?: boolean
}

/** I5: walk the tool list and bucket each tool into `allowed` / `denied` based on a declarative
 *  policy. A tool is denied when:
 *    - the tool name appears in `vetoes`
 *    - a `rules[i].pattern` matches the tool name and the rule's `action === "deny"`
 *    - or `defaultAction === "deny"` and no `allow` rule matches
 *  `ask_user` is treated as allowed at the schema layer — the runtime decides at call time.
 *  Pattern matching is exact match or a glob with a single trailing `*` (so `"write_*"` denies
 *  `write_file` and `write_db`). Pure — no side effects. */
export function governanceFilterSchema<T extends { name: string }>(
  tools: T[],
  policy: GovernancePolicy | undefined,
): { allowed: T[]; denied: string[] } {
  if (!policy) return { allowed: tools, denied: [] }
  const vetoes = new Set(policy.vetoes ?? [])
  const allowed: T[] = []
  const denied: string[] = []
  const matches = (pat: string, name: string): boolean =>
    pat === name || (pat.endsWith("*") && name.startsWith(pat.slice(0, -1)))
  for (const tool of tools) {
    if (vetoes.has(tool.name)) { denied.push(tool.name); continue }
    let action: GovernancePolicyAction = policy.defaultAction ?? "allow"
    for (const r of policy.rules ?? []) {
      if (matches(r.pattern, tool.name)) action = r.action
    }
    if (action === "deny") denied.push(tool.name)
    else allowed.push(tool)
  }
  return { allowed, denied }
}

export type GovernanceConstraint =
  | { kind: "required"; tool: string; path: string }
  | { kind: "enum"; tool: string; path: string; values: string[] }
  | { kind: "range"; tool: string; path: string; min?: number; max?: number }

/**
 * Convert a declarative {@link GovernancePolicy} into the `load_governance_policy`
 * kernel event payload (snake_case wire fields). Pure — no side effects.
 */
export function governancePolicyToKernelEvent(policy: GovernancePolicy): Record<string, unknown> {
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
        ? { kind: "enum", tool: c.tool, path: c.path, values: c.values }
        : c.kind === "range"
          ? { kind: "range", tool: c.tool, path: c.path, ...(c.min !== undefined ? { min: c.min } : {}), ...(c.max !== undefined ? { max: c.max } : {}) }
          : { kind: "required", tool: c.tool, path: c.path },
    ),
  }
}
