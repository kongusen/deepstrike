import { getKernel } from "./kernel.js"
import type { GovernanceInstance, GovernanceVerdict } from "./kernel.js"

type GovernanceDefaultAction = "allow" | "deny" | "ask_user"
type GovernanceRuleAction = "allow" | "deny" | "ask_user"

export class Governance {
  private readonly inner: GovernanceInstance

  constructor(defaultAction: GovernanceDefaultAction = "allow") {
    this.inner = new (getKernel().Governance)(defaultAction)
  }

  setIdentity(agentId: string, sessionId: string): void {
    this.inner.setIdentity(agentId, sessionId)
  }

  addPermissionRule(pattern: string, action: GovernanceRuleAction): void {
    this.inner.addPermissionRule(pattern, action)
  }

  blockTool(name: string): void {
    this.inner.blockTool(name)
  }

  setRateLimit(toolName: string, maxCalls: number, windowMs: number): void {
    this.inner.setRateLimit(toolName, maxCalls, BigInt(windowMs))
  }

  requireParam(toolName: string, paramPath: string): void {
    this.inner.requireParam(toolName, paramPath)
  }

  allowParamValues(toolName: string, paramPath: string, allowedValues: string[]): void {
    this.inner.allowParamValues(toolName, paramPath, allowedValues)
  }

  limitParamRange(toolName: string, paramPath: string, min?: number, max?: number): void {
    this.inner.limitParamRange(toolName, paramPath, min, max)
  }

  setTime(nowMs: number | bigint): void {
    this.inner.setTime(typeof nowMs === "bigint" ? nowMs : BigInt(nowMs))
  }

  evaluate(toolName: string, argsJson: string): GovernanceVerdict {
    return this.inner.evaluate(toolName, argsJson)
  }
}

export type { GovernanceVerdict }

// ─── Declarative policy (in-kernel gate) ──────────────────────────────────────
//
// The preferred way to configure governance: a plain data object the runner loads
// into the kernel via `load_governance_policy`, so the kernel enforces deny / veto /
// rate-limit / param-constraint before tools execute. The legacy `Governance` class
// above remains for the SDK-side gate (see COMPAT(gov-sdk-gate) markers).

type GovernancePolicyAction = "allow" | "deny" | "ask_user"

export interface GovernancePolicy {
  defaultAction?: GovernancePolicyAction
  rules?: { pattern: string; action: GovernancePolicyAction }[]
  vetoes?: string[]
  rateLimits?: { tool: string; maxCalls: number; windowMs: number }[]
  constraints?: GovernanceConstraint[]
  /** I5: when true (default), the runner pre-filters denied tools out of the schema passed to the
   *  provider — the model never sees them and never tries to call them, eliminating the rollback
   *  turn the kernel would otherwise produce. The denied tool names are also surfaced as a single
   *  line on the system slot so the model knows not to plan around them. Set to false to fall
   *  back to the v0.2.22 rollback-based behavior (useful for measuring the delta or when the
   *  agent should learn the denial via a real attempt). */
  surfaceDeniedInSystem?: boolean
  /** How the kernel surfaces a hard deny when the model does attempt the call:
   *  - `"rollback"` (default) — the turn is rolled back and a directive note re-prompts; the
   *    model never sees its own attempt.
   *  - `"result"` — the denial commits as an error tool result; the attempt stays visible in
   *    history and allowed sibling calls in the same batch still execute.
   *  Only observable with `surfaceDeniedInSystem: false` for statically denied tools (otherwise
   *  the schema pre-filter prevents the attempt); rate-limit / constraint denials are dynamic and
   *  always reach this path. */
  denyMode?: "rollback" | "result"
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
    ...(policy.denyMode ? { deny_mode: policy.denyMode } : {}),
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
