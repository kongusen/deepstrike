import { createRequire } from "module"
import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import type { Message, RenderedContext, ToolCall, ToolResult, ToolSchema, TaskUpdate } from "./types.js"
import type { SkillMetadata } from "./skills/loader.js"

export interface GovernanceVerdict {
  kind: "allow" | "deny" | "rate_limited" | "ask_user"
  reason?: string
  retryAfterMs?: number
}

export interface GovernanceInstance {
  setIdentity(agentId: string, sessionId: string): void
  addPermissionRule(pattern: string, action: "allow" | "deny" | "ask_user"): void
  blockTool(name: string): void
  setRateLimit(toolName: string, maxCalls: number, windowMs: bigint): void
  requireParam(toolName: string, paramPath: string): void
  allowParamValues(toolName: string, paramPath: string, allowedValues: string[]): void
  limitParamRange(toolName: string, paramPath: string, min?: number, max?: number): void
  setTime(nowMs: bigint): void
  evaluate(toolName: string, argsJson: string): GovernanceVerdict
}

export interface RuntimeSignal {
  id: string
  source: "cron" | "gateway" | "heartbeat" | "custom"
  signalType: "event" | "job" | "alert"
  urgency: "low" | "normal" | "high" | "critical"
  summary: string
  payload: string
  dedupeKey?: string
  timestampMs: number
}

export interface LoopAction {
  kind: "call_llm" | "execute_tools" | "evaluate_milestone" | "done"
  context?: RenderedContext
  tools?: ToolSchema[]
  calls?: ToolCall[]
  phase_id?: string
  criteria?: string[]
  result?: {
    termination: string
    turnsUsed: number
    totalTokensUsed: bigint
  }
}

interface LoopObservation {
  kind: "compressed" | "renewed" | "rollbacked" | "capability_changed" | "milestone_advanced" | "milestone_blocked"
  action?: string
  rhoAfter?: number
  sprint?: number
  summary?: string
  archived?: Message[]
  turn?: number
  checkpointHistoryLen?: number
  added?: string[]
  removed?: string[]
  phase_id?: string
  capabilities_unlocked?: string[]
  milestone_reason?: string
}

interface DeepStrikeRuntimeInstance {
  setAvailableSkills(skills: SkillMetadata[]): void
  setMemoryEnabled(enabled: boolean): void
  setKnowledgeEnabled(enabled: boolean): void
  addSystemMessage(content: string, tokens: number): void
  addMemoryMessage(content: string, tokens: number): void
  addHistoryMessage(message: Message, tokens: number): void
  setTools(tools: ToolSchema[]): void
  mountTool(schema: ToolSchema): void
  mountSkill(skill: SkillMetadata): void
  mountMarker(kind: string, id: string, description: string): void
  unmountCapability(kind: string, id: string): void
  start(task: { goal: string; criteria: string[] }): LoopAction
  resumeAfterPreload(): LoopAction
  feedLlmResponse(message: Message): LoopAction
  feedToolResults(results: ToolResult[]): LoopAction
  feedTimeout(): LoopAction
  isTerminal(): boolean
  preloadHistory(messages: Message[]): void
  drainNewMessages(): Message[]
  readonly turn: number
  pressure(): number
  takeObservations(): LoopObservation[]
  forceCompact(): boolean
  force_compact?(): boolean
  render(): RenderedContext
  initTask(goal: string, criteria: string[]): void
  updateTask(update: TaskUpdate): void
  recoveryContentBytes(): number
  setTokenizer(name: string): void
  setPlanToolEnabled(enabled: boolean): void
}

interface SignalRouterInstance {
  ingest(signal: RuntimeSignal, isRunning: boolean): string
  next(): RuntimeSignal | null
}

interface NativeCriterion {
  text: string
  required: boolean
  weight?: number
}

interface EvalPipelineAction {
  kind: "evaluate" | "done"
  messages?: Message[]
  passed?: boolean
  overallScore?: number
  feedback?: string
  details?: Array<{
    criterion: string
    passed: boolean
    score: number
    feedback: string
  }>
  skillCandidate?: {
    name: string
    description: string
    whenToUse?: string
    content: string
  }
}

interface EvalPipelineInstance {
  feedOutcome(goal: string, criteria: NativeCriterion[], result: string, attempt: number): EvalPipelineAction
  feedEvalResult(content: string): EvalPipelineAction
  reset(): void
}

interface IdlePipelineAction {
  kind: "synthesize_insights" | "commit_memories" | "noop" | "aborted"
  messages?: Message[]
  curationResult?: {
    toAdd?: Array<{ text: string; score: number; metadata: string }>
    toRemoveIndices?: number[]
    stats?: {
      insightsProcessed?: number
      duplicatesRemoved?: number
      conflictsResolved?: number
      entriesAdded?: number
    }
  }
  runResult?: {
    sessionsProcessed: number
    insightsExtracted: number
  }
}

interface IdlePipelineInstance {
  feedTrigger(
    sessions: Array<{
      sessionId: string
      agentId: string
      messages: Message[]
      metadata: string
      createdAtMs: number
      updatedAtMs: number
    }>,
    existingMemories: Array<{ text: string; score: number; metadata: string }>,
    nowMs: number,
  ): IdlePipelineAction
  feedSynthesisResult(content: string): IdlePipelineAction
}

interface KernelModule {
  Governance: new (defaultAction?: "allow" | "deny" | "ask_user") => GovernanceInstance
  DeepStrikeRuntime: new (policy: {
    maxTokens: number
    maxTurns?: number
    maxTotalTokens?: bigint
    timeoutMs?: bigint
  }) => DeepStrikeRuntimeInstance
  SignalRouter: new (maxQueueSize: number) => SignalRouterInstance
  EvalPipeline: new (options?: { extractSkillOnPass?: boolean }) => EvalPipelineInstance
  IdlePipeline: new (agentId: string) => IdlePipelineInstance
}

const cjsRequire = createRequire(import.meta.url)
let cachedKernel: KernelModule | undefined

function resolveCoreModule(): string {
  const localCore = join(dirname(fileURLToPath(import.meta.url)), "../../crates/deepstrike-node")
  if (existsSync(join(localCore, "index.js"))) return localCore
  return "@deepstrike/core"
}

export function getKernel(): KernelModule {
  if (!cachedKernel) {
    cachedKernel = cjsRequire(resolveCoreModule()) as KernelModule
  }
  return cachedKernel
}
