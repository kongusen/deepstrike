/* eslint-disable */
export interface Message { role: string; content: string; tokenCount?: number; toolCalls: ToolCall[] }
export interface ToolCall { id: string; name: string; arguments: string }
export interface ToolResult { callId: string; output: string; isError: boolean; tokenCount?: number }
export interface ToolSchema { name: string; description: string; parameters: string }
export interface RuntimeTask { goal: string; criteria: string[] }
export interface LoopPolicy { maxTokens: number; maxTurns?: number; maxTotalTokens?: bigint; timeoutMs?: bigint }
export interface LoopResult { termination: string; finalMessage?: Message; turnsUsed: number; totalTokensUsed: bigint }
export interface RuntimeSignal { id: string; source: string; signalType: string; urgency: string; summary: string; payload: string; dedupeKey?: string; timestampMs: number }
export interface SkillMetadata { name: string; description: string; whenToUse?: string; allowedTools?: string[]; effort?: number; estimatedTokens: number }
export interface LoopAction { kind: string; messages?: Message[]; tools?: ToolSchema[]; calls?: ToolCall[]; result?: LoopResult }
export interface LoopObservation { kind: string; action?: string; rhoAfter?: number }
export interface SkillCandidate { name: string; description: string; whenToUse?: string; content: string }
export interface EvalPipelineAction { kind: string; messages?: Message[]; passed?: boolean; feedback?: string; skillCandidate?: SkillCandidate }
export interface GovernanceVerdict { kind: 'allow' | 'deny' | 'rate_limited' | 'ask_user'; reason?: string }

export class ContextEngine {
  constructor(maxTokens: number)
  addSystemMessage(content: string, tokens: number): void
  addUserMessage(content: string, tokens: number): void
  addAssistantMessage(content: string, tokens: number): void
  pressure(): number; totalTokens(): number; compress(): number; render(): Message[]
  setAvailableSkills(skills: SkillMetadata[]): void
}

export class LoopStateMachine {
  constructor(policy: LoopPolicy)
  setAvailableSkills(skills: SkillMetadata[]): void
  setMemoryEnabled(enabled: boolean): void
  setKnowledgeEnabled(enabled: boolean): void
  setTools(tools: ToolSchema[]): void
  start(task: RuntimeTask): LoopAction
  feedLlmResponse(message: Message): LoopAction
  feedToolResults(results: ToolResult[]): LoopAction
  feedTimeout(): LoopAction
  isTerminal(): boolean
  turn: number
  pressure(): number
  takeObservations(): LoopObservation[]
  render(): Message[]
}

export class SignalRouter {
  constructor(maxQueueSize: number)
  ingest(signal: RuntimeSignal, isRunning: boolean): string
  next(): RuntimeSignal | null
  depth(): number
  clearDedup(): void
}

export class Governance {
  constructor()
  blockTool(name: string): void
  setTime(nowMs: bigint): void
  evaluate(toolName: string, args: string): GovernanceVerdict
}

export class EvalPipeline {
  constructor(options?: { extractSkillOnPass?: boolean })
  feedOutcome(goal: string, criteria: string[], result: string, attempt: number): EvalPipelineAction
  feedEvalResult(content: string): EvalPipelineAction
  reset(): void
  isIdle(): boolean
}

export class IdlePipeline {
  constructor(agentId: string)
  feedTrigger(sessions: unknown[], existingMemories: unknown[], nowMs: number): unknown
  feedSynthesisResult(content: string): unknown
  isIdle(): boolean
  reset(): void
}
