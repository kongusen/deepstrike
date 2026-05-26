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
