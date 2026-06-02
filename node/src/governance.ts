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
