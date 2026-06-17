export interface GovernanceVerdict {
  kind: "allow" | "deny" | "rate_limited" | "ask_user"
  reason?: string
  retryAfterMs?: number
}

type DefaultAction = "allow" | "deny" | "ask_user"
type RuleAction = "allow" | "deny" | "ask_user"

export class Governance {
  private _inner: import("@deepstrike/wasm-kernel").Governance | null = null
  private _defaultAction: DefaultAction
  private _pendingCalls: Array<(g: import("@deepstrike/wasm-kernel").Governance) => void> = []

  constructor(defaultAction: DefaultAction = "allow") {
    this._defaultAction = defaultAction
  }

  /** Called by Agent after the WASM kernel module is loaded. */
  _attach(kernel: typeof import("@deepstrike/wasm-kernel")): void {
    if (this._inner) return
    this._inner = new kernel.Governance(this._defaultAction)
    for (const fn_ of this._pendingCalls) fn_(this._inner)
    this._pendingCalls = []
  }

  private _apply(fn: (g: import("@deepstrike/wasm-kernel").Governance) => void): this {
    if (this._inner) fn(this._inner)
    else this._pendingCalls.push(fn)
    return this
  }

  setIdentity(agentId: string, sessionId: string): this {
    return this._apply(g => g.setIdentity(agentId, sessionId))
  }

  addPermissionRule(pattern: string, action: RuleAction): this {
    return this._apply(g => g.addPermissionRule(pattern, action))
  }

  blockTool(name: string): this {
    return this._apply(g => g.blockTool(name))
  }

  setRateLimit(toolName: string, maxCalls: number, windowMs: number): this {
    return this._apply(g => g.setRateLimit(toolName, maxCalls, windowMs))
  }

  requireParam(toolName: string, paramPath: string): this {
    return this._apply(g => g.requireParam(toolName, paramPath))
  }

  allowParamValues(toolName: string, paramPath: string, allowedValues: string[]): this {
    return this._apply(g => g.allowParamValues(toolName, paramPath, allowedValues))
  }

  limitParamRange(toolName: string, paramPath: string, min?: number, max?: number): this {
    return this._apply(g => g.limitParamRange(toolName, paramPath, min, max))
  }

  setTime(nowMs: number): this {
    return this._apply(g => g.setTime(nowMs))
  }

  evaluate(toolName: string, argsJson: string): GovernanceVerdict {
    if (!this._inner) return { kind: "allow" }
    return this._inner.evaluate(toolName, argsJson) as GovernanceVerdict
  }
}

// ─── Declarative policy (in-kernel gate) ──────────────────────────────────────

type GovernancePolicyAction = "allow" | "deny" | "ask_user"

export interface GovernancePolicy {
  defaultAction?: GovernancePolicyAction
  rules?: { pattern: string; action: GovernancePolicyAction }[]
  vetoes?: string[]
  rateLimits?: { tool: string; maxCalls: number; windowMs: number }[]
  constraints?: GovernanceConstraint[]
  /** I5: when true (default), the runner pre-filters denied tools out of the schema. */
  surfaceDeniedInSystem?: boolean
}

/** I5: bucket tools into allowed/denied per the policy. Pure. Mirrors Node. */
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
