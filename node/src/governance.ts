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
