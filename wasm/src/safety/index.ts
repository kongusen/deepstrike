export enum PermissionMode { DEFAULT = "DEFAULT", PLAN = "PLAN", AUTO = "AUTO" }

export interface PermissionDecision { allowed: boolean; reason: string }

export class PermissionManager {
  private grants = new Map<string, Set<string>>()
  constructor(private mode: PermissionMode = PermissionMode.DEFAULT) {}

  grant(resource: string, action: string): this {
    if (!this.grants.has(resource)) this.grants.set(resource, new Set())
    this.grants.get(resource)!.add(action)
    return this
  }

  revoke(resource: string, action: string): this {
    this.grants.get(resource)?.delete(action)
    return this
  }

  evaluate(resource: string, action: string): PermissionDecision {
    if (this.mode === PermissionMode.AUTO) return { allowed: true, reason: "AUTO mode" }
    if (this.mode === PermissionMode.PLAN) return { allowed: false, reason: "PLAN mode blocks all" }
    const actions = this.grants.get(resource)
    const allowed = !!(actions && (actions.has(action) || actions.has("*")))
    return { allowed, reason: allowed ? "granted" : "not granted" }
  }
}
