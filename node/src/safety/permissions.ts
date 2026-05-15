export enum PermissionMode {
  DEFAULT = "DEFAULT",
  PLAN = "PLAN",
  AUTO = "AUTO",
}

export interface Permission {
  tool: string
  action: string
  allowed: boolean
  requiresApproval: boolean
  note: string
}

export interface PermissionDecision {
  allowed: boolean
  reason: string
  requiresApproval?: boolean
  matchedPermission?: Permission
}

export class PermissionManager {
  private permissions = new Map<string, Permission>()

  constructor(private mode: PermissionMode = PermissionMode.DEFAULT) {}

  grant(resource: string, action: string, options: { requiresApproval?: boolean; note?: string } = {}): this {
    const key = `${resource}:${action}`
    this.permissions.set(key, {
      tool: resource, action, allowed: true,
      requiresApproval: options.requiresApproval ?? false,
      note: options.note ?? "",
    })
    return this
  }

  grantWithApproval(resource: string, action: string, note = ""): this {
    return this.grant(resource, action, { requiresApproval: true, note })
  }

  revoke(resource: string, action: string, note = ""): this {
    const key = `${resource}:${action}`
    this.permissions.set(key, { tool: resource, action, allowed: false, requiresApproval: false, note })
    return this
  }

  private matchPermission(tool: string, action: string): Permission | undefined {
    for (const key of [`${tool}:${action}`, `${tool}:*`, `*:${action}`, `*:*`]) {
      const p = this.permissions.get(key)
      if (p) return p
    }
    return undefined
  }

  evaluate(resource: string, action: string): PermissionDecision {
    if (this.mode === PermissionMode.AUTO) return { allowed: true, reason: "AUTO mode" }
    if (this.mode === PermissionMode.PLAN) return { allowed: false, reason: "PLAN mode blocks all" }
    const perm = this.matchPermission(resource, action)
    if (!perm) return { allowed: false, reason: "not granted" }
    if (!perm.allowed) return { allowed: false, reason: perm.note || "permission denied", matchedPermission: perm }
    if (perm.requiresApproval) return { allowed: false, reason: perm.note || "requires approval", requiresApproval: true, matchedPermission: perm }
    return { allowed: true, reason: "granted", matchedPermission: perm }
  }
}
