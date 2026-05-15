export interface GovernanceVerdict {
  kind: "allow" | "deny" | "rate_limited" | "ask_user"
  reason?: string
  retryAfterMs?: number
}

export class Governance {
  private _inner: import("@deepstrike/wasm-kernel").Governance | null = null
  private _pendingBlocks: string[] = []

  /** Called by Agent after the WASM kernel module is loaded. */
  _attach(kernel: typeof import("@deepstrike/wasm-kernel")): void {
    if (this._inner) return
    this._inner = new kernel.Governance()
    for (const name of this._pendingBlocks) this._inner.blockTool(name)
  }

  blockTool(name: string): this {
    if (this._inner) this._inner.blockTool(name)
    else this._pendingBlocks.push(name)
    return this
  }

  setTime(nowMs: number): this {
    this._inner?.setTime(nowMs)
    return this
  }

  evaluate(toolName: string, argsJson: string): GovernanceVerdict {
    if (!this._inner) return { kind: "allow" }
    return this._inner.evaluate(toolName, argsJson)
  }
}
