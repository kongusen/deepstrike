import type {
  LLMProvider, Message, ContentPart, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, WorkflowNodesSubmittedEvent, DoneEvent, ErrorEvent,
  ToolSuspendEvent, ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent,
  PermissionResponse, PermissionResolvedEvent, AsyncSummarizer, DreamSummarizer,
} from "../types.js"
import type {
  DreamStore,
  MemoryEntry,
  CurationResult,
  SessionData,
  MemoryQuery,
  MemoryWriteRequest,
  MemoryRetrieval,
} from "../memory/protocols.js"
import { memoriesToIndex, selectMemories } from "../memory/agent.js"
import type { KnowledgeSource } from "../knowledge/source.js"
import type { SignalSource, RuntimeSignal } from "../signals/types.js"
import type { SessionLog, SessionEvent, RollbackReason } from "./session-log.js"
import type { ArchiveStore } from "./archive.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { resolvePermissionRequest } from "./execution-plane.js"
import { getKernel, type KernelRuntimeInstance, type MemoryPolicy, type ResourceQuota } from "../kernel.js"
import { peekProviderReplay, seedProviderReplayFromEvents } from "./provider-replay.js"
import { sanitizeReplayText } from "./replay-sanitize.js"
import {
  buildLlmCompletedEvent,
  buildRunTerminalEvent,
  buildWorkflowNodeCompletedEvent,
  buildWorkflowNodesSubmittedEvent,
  recoverCompletedWorkflowNodes,
  recoverSubmittedWorkflowNodes,
  repairEventsForRecovery,
} from "./session-repair.js"
import { KernelPrimitivesDashboard } from "./kernel-primitives-dashboard.js"
import {
  capabilityMarker,
  capabilitySkill,
  capabilityTool,
  capabilityCommandMount,
  capabilityCommandUnmount,
  kernelAction,
  kernelApply,
  kernelMaybeAction,
  forceCompact,
  messageToKernelMessage,
  skillMetadataToKernel,
  taskUpdateToKernel,
  toolResultToKernel,
  toolSchemaToKernel,
  type KernelObservation,
  type KernelRunnerAction,
} from "./kernel-step.js"
import type {
  AgentRunSpec, AgentProcessChangedObservation, MilestoneCheckResult, MilestoneContract, MilestonePolicy, SubAgentResult,
  WorkflowSpec, WorkflowSpawnInfo, WorkflowNodeSpec, WorkflowBudget,
} from "../types/agent.js"
import {
  agentRunSpecToKernel,
  findSpawnProcessObservation,
  milestoneCheckPass,
  milestoneCheckResultToKernel,
  spawnObservationToManifest,
  subAgentResultToKernel,
  submitWorkflowNodesToKernel,
  submitWorkflowToKernel,
  workflowBudgetNote,
  workflowNodeToManifest,
  workflowNodeToSpec,
  workflowSpecToKernel,
} from "../types/agent.js"
import { defaultSubAgentOrchestrator, type SubAgentOrchestrator } from "./sub-agent-orchestrator.js"
import {
  extractJsonValue,
  schemaInstruction,
  schemaRetryInstruction,
  validateAgainstSchema,
} from "./output-schema.js"
import { resolveReducer, type ReducerRegistry } from "./reducers.js"
import {
  loopInstruction, classifyInstruction, judgeGoal,
  extractLoopContinue, extractClassifyBranch, extractJudgeWinner,
} from "./workflow-control-flow.js"
import { governancePolicyToKernelEvent, governanceFilterSchema, type GovernancePolicy } from "../governance.js"
import { kernelObservationToSessionEvent, withCategory } from "./kernel-event-log.js"
import { assertNativeProfile, type NativeOsProfile, type OsProfileId } from "./os-profile.js"
import { LargeResultSpool } from "./large-result-spool.js"

export interface SchedulerBudget {
  maxWallMs?: number
}

/** P0-C tool-gating telemetry: per-LLM-turn metrics, emitted via `RuntimeOptions.onTurnMetrics`.
 *  Pure observation — no behavior change. Feeds the go/no-go analysis for epoch skill gating (P1-B):
 *  - `toolsExposed` vs `toolsCalled` quantifies over-exposure.
 *  - `activeSkill` across consecutive turns yields the skill *dwell* `D` (how long a skill stays
 *    loaded) — the break-even input that decides whether dynamic gating beats the cache-bust cost.
 *  - `cacheReadTokens` / `cacheCreationTokens` give the prompt-cache hit baseline to compare against
 *    after B/D ship. */
export interface TurnMetrics {
  /** 1-based kernel turn this LLM call belongs to. */
  turn: number
  /** Number of tool schemas exposed to the model this turn (base + meta, after run-profile gating). */
  toolsExposed: number
  /** Number of tool calls the model emitted this turn. */
  toolsCalled: number
  /** The skill loaded and in effect going into this turn (the most recent `skill` tool call's name),
   *  or undefined if none is active. Consecutive equal values measure dwell. */
  activeSkill?: string
  /** Full prompt size the provider reported (uncached + cache read + cache creation). */
  inputTokens: number
  /** Tokens served from the prompt cache this turn (Anthropic `cache_read_input_tokens`). */
  cacheReadTokens: number
  /** I1: per-slot attribution of `cacheReadTokens`. Anthropic reports a single cache-read total,
   *  not a per-block breakdown — this field is a pro-rata estimate over the slots that actually
   *  carried a `cache_control` breakpoint on the request. Missing / empty when the provider does
   *  not honor `cache_control` (OpenAI-family auto-cache) or when no breakpoints were placed.
   *  Useful for diagnosing which slot is buying the cache hit when comparing strategies. */
  cacheReadTokensBySlot?: { system?: number; tools?: number; messages?: number }
  /** Tokens written to the prompt cache this turn (Anthropic `cache_creation_input_tokens`). */
  cacheCreationTokens: number
}

export interface RuntimeOptions {
  provider: LLMProvider
  /** M4/G5: cumulative token cap for this run (the kernel's `max_total_tokens`). A workflow node's
   *  `tokenBudget` flows here for its child run, so an expensive node self-terminates at the cap.
   *  Undefined ⇒ the kernel default. */
  maxTotalTokens?: number
  /** M1/G3 intelligence routing: resolve a per-node provider from a workflow node's `modelHint`
   *  (e.g. "opus" / "sonnet" / "haiku"). Returns undefined ⇒ fall back to `provider`. A workflow
   *  node carrying `model_hint` runs against the resolved provider; without this hook the hint is a
   *  no-op (the kernel still carries it for audit). */
  providerFor?: (modelHint: string) => LLMProvider | undefined
  /** M3/G4 worktree isolation: when set, an `isolation: "worktree"` sub-agent runs inside a git
   *  worktree this manager creates (and removes on completion), injected as `RunContext.cwd`.
   *  Undefined ⇒ worktree nodes fall back to the inherited plane (no isolation). */
  worktreeManager?: import("./worktree-plane.js").WorktreeManager
  sessionLog: SessionLog
  executionPlane: ExecutionPlane
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  agentId?: string
  /** I4: optional run-start memory pre-fetch hook. The runner calls this ONCE per run, before the
   *  first LLM turn, with the request's goal and (optional) run-spec. Each returned query string
   *  becomes a `dreamStore.search(agentId, q, 5)` and the resulting hits are paged into the
   *  context's knowledge partition before turn 1, so the model sees them on first call. Returning
   *  `undefined` / empty array is a no-op. Requires `dreamStore` + `agentId`; missing either ⇒
   *  silently skipped (errs-open). Bench memory-recall shows -57% turns / -55% dollars when
   *  relevant memories land on turn 1 instead of being discovered via the meta-tool on turn 3+. */
  preQueryMemory?: (ctx: { goal: string; runSpec?: AgentRunSpec }) => Promise<string[] | undefined> | string[] | undefined
  systemPrompt?: string
  initialMemory?: string[]
  skillDir?: string
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  extensions?: Record<string, unknown>
  /** Named or concrete OS profile. Defaults to the native microkernel profile. */
  osProfile?: OsProfileId | NativeOsProfile
  /**
   * Declarative governance policy loaded into the kernel (`load_governance_policy`).
   * The kernel enforces deny/veto/rate-limit/param-constraint before tools execute;
   * AskUser calls surface as `tool_gated` and run through `onPermissionRequest`.
   */
  governancePolicy?: GovernancePolicy
  /**
   * Enable in-kernel signal routing (`set_attention_policy`). When set, inbound
   * signals are dispatched through the kernel attention policy (dedup + disposition
   * + queue) and surface as `signal_disposed` observations, instead of the legacy
   * SDK-side router. `maxQueueSize` defaults to 64.
   */
  attentionPolicy?: { maxQueueSize?: number }
  /**
   * Optional scheduler budget overrides. `maxWallMs` is the wall-clock run budget
   * in milliseconds; when set, the kernel terminates the run when exceeded.
   * Other axes (maxTurns, maxTokens) are set via RuntimeOptions directly.
   */
  schedulerBudget?: SchedulerBudget
  /**
   * Optional declarative resource quotas (`set_resource_quota`). Bounds spawn concurrency /
   * nesting depth and memory-write rate at the kernel's single syscall trap. When unset, spawn
   * and memory-write syscalls are admitted unconditionally (pre-M2 behavior).
   */
  resourceQuota?: ResourceQuota
  /**
   * Optional long-term memory policy (`set_memory_policy`). Tunes the kernel's memory subsystem
   * (retrieval top-k, stale-warning age, write validation, memory path). Unset leaves the kernel
   * defaults. Enabling memory still requires `dreamStore` + `agentId`.
   */
  memoryPolicy?: MemoryPolicy
  tokenizer?: string
  enablePlanTool?: boolean
  /**
   * Persist full tool outputs when the kernel emits `large_result_spooled`.
   * Defaults to `.spool/` under the process cwd.
   */
  resultSpool?: LargeResultSpool
  compressionStore?: ArchiveStore
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
  onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
  /** Default: terminate — stop with milestone_pending when a phase needs evaluation. */
  milestonePolicy?: MilestonePolicy
  /** Optional external verifier when milestonePolicy is not auto_pass. */
  onMilestoneEvaluate?: (ctx: {
    phaseId: string
    criteria: string[]
    requiredEvidence: string[]
  }) => Promise<MilestoneCheckResult> | MilestoneCheckResult
  /** Passed to kernel start_run for role/isolation metadata. */
  runSpec?: AgentRunSpec
  /** P0-A tool gating: a static per-run tool profile — only these tool ids (plus the
   *  skill/memory/knowledge/update_plan meta-tools) are exposed to the model each turn.
   *  Sugar that lowers to the same `capability_filter` sub-agents use; byte-stable across
   *  the run, so it never busts the prompt-cache prefix. Augments `runSpec`'s filter when
   *  both are set; synthesizes a minimal run spec when `runSpec` is absent. Omitted/empty
   *  ⇒ all registered tools exposed (no gating). */
  allowedToolIds?: string[]
  /** P0-C: optional per-turn metrics sink for tool-gating telemetry (see `TurnMetrics`). Pure
   *  observation; invoked once per LLM turn. Never throws into the run loop (errors are swallowed). */
  onTurnMetrics?: (metrics: TurnMetrics) => void
  /** P1-B/D stable-core: tool ids that stay exposed even when an active skill narrows the toolset
   *  (read/search/bash etc.). Empty/absent ⇒ skills narrow to exactly their declared `allowed_tools`
   *  + meta-tools. Opt-in: with no skill declaring `allowed_tools`, gating never engages. */
  stableCoreToolIds?: string[]
  /** Loaded via load_milestone_contract before run start. */
  milestoneContract?: MilestoneContract
  /** Custom sub-agent host driver; defaults to SubAgentOrchestrator. */
  subAgentOrchestrator?: SubAgentOrchestrator
  /** M5 v2.1: marks this runner as executing AS a workflow node (a child spawned by the workflow
   *  driver). A workflow node's `start_workflow` FLATTENS to the parent kernel (emits
   *  `workflow_nodes_submitted` for `runWorkflow` to append). A top-level run (this flag unset)
   *  instead AUTO-PIVOTS: it bootstraps + drives the authored workflow in its own kernel and resumes
   *  the reason loop with the outcome. The orchestrator sets this on workflow-node children so a
   *  nested `start_workflow` flattens rather than recursing. */
  isWorkflowNode?: boolean
  /**
   * When set, sub-agents run through a HarnessLoop with this config.
   * The eval provider evaluates the sub-agent's output against the criteria
   * from the AgentRunSpec, retrying up to maxAttempts times.
   */
  subAgentHarness?: {
    evalProvider: LLMProvider
    maxAttempts?: number
  }
  /** G2: custom reducers for `NodeKind::Reduce` workflow nodes, merged over the built-ins
   *  (`concat` / `dedupe_lines` / `merge_json_arrays` / `count`). A reduce node runs no LLM. */
  reducers?: ReducerRegistry
  /** Optional system prompt injected into the dream synthesis call. */
  dreamSystemPrompt?: string
  /** Custom LLM provider used for background memory consolidation (dream loop). */
  dreamProvider?: LLMProvider
  /**
   * Optional LLM summarizer for semantic page_out events. When unset, `dreamProvider`
   * (or the runtime provider) is used to produce long-term summaries for DreamStore.
   */
  dreamSummarizer?: DreamSummarizer
  /**
   * Optional async LLM summarizer. When provided, a background call is fired
   * after each compression event to produce a richer semantic summary.
   * The result is written back to SessionLog as `summary_upgraded` and used
   * on the next wake() in place of the rule-based summary.
   */
  asyncSummarizer?: AsyncSummarizer
  /** Enable real-time CLI diagnostics dashboard grouped by the three kernel primitives (Syscall, Sched, Mm) */
  enableDiagnosticsDashboard?: boolean
}

export class RuntimeRunner {
  private interrupted = false
  /** #2-B-ii: aborts the in-flight provider stream when the run is interrupted/preempted. Recreated
   *  per `execute`; `interrupt()` fires it so a Critical `InterruptNow` cancels the live LLM call. */
  private abortController: AbortController | null = null
  private activeKernel: KernelRuntimeInstance | null = null
  private pendingObservations: KernelObservation[] = []
  private currentSessionId: string | null = null
  private nextArchiveStart = 0
  /** Full tool outputs keyed by call_id until Layer-1 spool observations are logged. */
  private pendingSpoolOutputs = new Map<string, { tool: string; output: string }>()
  /** Local cache of paged-out/archived messages for priority memory retrieval. */
  private localPageOutCache: Message[] = []
  /** M5 v2.1: sub-workflow specs a top-level agent authored via `start_workflow`, awaiting auto-drive
   *  at the next safe point (after the tool turn resolves, kernel back in Reason — not suspended). */
  private pendingAuthoredWorkflows: WorkflowSpec[] = []
  private dashboard: KernelPrimitivesDashboard | null = null

  constructor(private readonly opts: RuntimeOptions) {
    if (opts.enableDiagnosticsDashboard) {
      const originalAppend = opts.sessionLog.append.bind(opts.sessionLog)
      opts.sessionLog.append = async (sessionId, event) => {
        const seq = await originalAppend(sessionId, event)
        if (this.dashboard) {
          this.dashboard.ingest(event)
          this.dashboard.print()
        }
        return seq
      }
    }
  }

  /** Host configuration (for coordinator / sub-agent spawn). */
  get hostOptions(): RuntimeOptions {
    return this.opts
  }

  async writeMemory(
    memory: MemoryWriteRequest,
    opts: { sessionId?: string; agentId?: string } = {},
  ): Promise<void> {
    const sessionId = opts.sessionId ?? this.currentSessionId
    const agentId = opts.agentId ?? this.opts.agentId
    if (!this.opts.dreamStore || !agentId) return

    const observations: KernelObservation[] = []
    const runtime = this.activeKernel ?? this.createSyscallRuntime()
    kernelApply(runtime, observations, { kind: "write_memory", memory })

    const event = observations.find(o => o.kind === "memory_written")
    if (!event) {
      await this.appendMemorySyscallObservations(sessionId, observations)
      return
    }

    const existing = await this.opts.dreamStore.loadMemories(agentId)
    await this.opts.dreamStore.commit(agentId, {
      toAdd: [{
        text: memory.content,
        score: 1.0,
        metadata: {
          ...memory.metadata,
          source: "write_memory_syscall",
        },
      }],
      toRemoveIndices: [],
      stats: {
        insightsProcessed: 1,
        duplicatesRemoved: 0,
        conflictsResolved: 0,
        entriesAdded: 1,
      },
    }, existing)
    await this.appendMemorySyscallObservations(sessionId, observations)
  }

  async queryMemory(
    query: MemoryQuery,
    opts: { sessionId?: string; agentId?: string } = {},
  ): Promise<MemoryEntry[]> {
    const sessionId = opts.sessionId ?? this.currentSessionId
    const agentId = opts.agentId ?? this.opts.agentId
    if (!this.opts.dreamStore || !agentId) return []

    const observations: KernelObservation[] = []
    const runtime = this.activeKernel ?? this.createSyscallRuntime()
    kernelApply(runtime, observations, { kind: "query_memory", query })

    const allMemories = await this.opts.dreamStore.loadMemories(agentId)
    const retrieval = await selectMemories(query, memoriesToIndex(allMemories))
    let hits: MemoryEntry[]
    if (retrieval.selected_memory_ids.length > 0) {
      const selected = new Set(retrieval.selected_memory_ids)
      hits = allMemories
        .filter(m => selected.has(String((m.metadata as Record<string, unknown>)?.name ?? "")))
        .slice(0, query.top_k)
    } else {
      hits = await this.opts.dreamStore.search(agentId, query.current_context, query.top_k)
      if (hits.length > 0 && retrieval.selection_rationale === "No candidates after filtering") {
        retrieval.selected_memory_ids = hits.map(h =>
          String((h.metadata as Record<string, unknown>)?.name ?? h.text.slice(0, 32)),
        )
        retrieval.selection_rationale = `DreamStore.search returned ${hits.length} hit(s)`
      }
    }

    await this.appendMemorySyscallObservations(sessionId, observations)
    await this.logMemoryRetrievalResult(sessionId, runtime, retrieval)
    return hits
  }

  private async logMemoryRetrievalResult(
    sessionId: string | null | undefined,
    runtime: KernelRuntimeInstance,
    retrieval: MemoryRetrieval,
  ): Promise<void> {
    if (!sessionId) return
    await this.opts.sessionLog.append(sessionId, {
      kind: "memory_retrieval_result",
      selected_memory_ids: retrieval.selected_memory_ids,
      selection_rationale: retrieval.selection_rationale,
    })
    kernelApply(runtime, [], {
      kind: "memory_retrieval_result",
      retrieval: {
        selected_memory_ids: retrieval.selected_memory_ids,
        selection_rationale: retrieval.selection_rationale,
      },
    })
  }

  private createSyscallRuntime(): KernelRuntimeInstance {
    const { KernelRuntime } = getKernel()
    return new KernelRuntime({
      maxTokens: this.opts.maxTokens,
      maxTurns: this.opts.maxTurns,
      // M4/G5: per-node token cap → child run's cumulative token budget.
      maxTotalTokens: this.opts.maxTotalTokens !== undefined ? BigInt(this.opts.maxTotalTokens) : undefined,
      timeoutMs: this.opts.timeoutMs !== undefined ? BigInt(this.opts.timeoutMs) : undefined,
    })
  }

  private async appendMemorySyscallObservations(
    sessionId: string | null | undefined,
    observations: KernelObservation[],
  ): Promise<void> {
    if (!sessionId) return
    const turn = this.activeKernel?.turn() ?? 0
    for (const obs of observations) {
      if (
        obs.kind !== "memory_written"
        && obs.kind !== "memory_queried"
        && obs.kind !== "memory_validation_failed"
      ) continue
      const event = kernelObservationToSessionEvent(obs, turn)
      if (event) await this.opts.sessionLog.append(sessionId, event)
    }
  }

  /** Mount a tool capability on the currently-running kernel runtime. No-op if not running. */
  mountTool(schema: ToolSchema): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(capabilityTool(schema)))
  }

  /** Mount a skill capability on the currently-running kernel runtime. No-op if not running. */
  mountSkill(name: string, description: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(
      capabilitySkill({ name, description, estimatedTokens: 0 }),
    ))
  }

  /** Mount a generic marker capability (e.g. MCP server, agent) on the active run. No-op if not running. */
  mountMarker(kind: string, id: string, description: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(
      capabilityMarker(kind, id, description),
    ))
  }

  /** Unmount a capability by kind + id from the active run. No-op if not running. */
  unmountCapability(kind: string, id: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandUnmount(kind, id))
  }

  /** Phase 4: satisfy kernel page-in requests before meta-tool execution. */
  private async applyKernelPageIn(
    runtime: KernelRuntimeInstance,
    sessionId: string,
  ): Promise<void> {
    const requests = this.pendingObservations.filter(
      (o): o is KernelObservation & { kind: "page_in_requested"; tool: string; query: string; top_k?: number } =>
        o.kind === "page_in_requested" && typeof o.tool === "string",
    )
    if (requests.length === 0) return

    const entries: Array<{ content: string; tokens?: number; source?: string }> = []
    for (const req of requests) {
      const query = typeof req.query === "string" ? req.query : ""
      const topK = typeof req.top_k === "number" ? req.top_k : 5
      if (req.tool === "memory") {
        // Priority search: Local Page-Out Cache (lexical/keyword filter)
        const localHits = this.localPageOutCache.filter(m =>
          typeof m.content === "string" && m.content.toLowerCase().includes(query.toLowerCase())
        ).slice(0, topK)

        for (const hit of localHits) {
          entries.push({
            content: `[local semantic cache] ${hit.role}: ${hit.content}`,
            source: "semantic_cache",
          })
        }

        // Fall back to dreamStore for the remainder if needed
        const remainingK = topK - entries.length
        if (remainingK > 0 && this.opts.dreamStore && this.opts.agentId) {
          const hits = await this.opts.dreamStore.search(this.opts.agentId, query, remainingK)
          for (const hit of hits) {
            entries.push({
              content: `[memory score=${hit.score.toFixed(3)}] ${hit.text}`,
              source: "memory",
            })
          }
        }
      } else if (req.tool === "knowledge" && this.opts.knowledgeSource) {
        const snippets = await this.opts.knowledgeSource.retrieve(query, topK)
        for (const snippet of snippets) {
          entries.push({ content: snippet, source: "knowledge" })
        }
      }
    }
    if (entries.length === 0) return
    kernelApply(runtime, this.pendingObservations, { kind: "page_in", entries })
    await this.opts.sessionLog.append(sessionId, withCategory({
      kind: "page_in",
      turn: runtime.turn(),
      entry_count: entries.length,
    }))
  }

  /** Push content into the Knowledge slot (memory retrievals, skill definitions, artifacts). */
  pushKnowledge(message: Message, tokens?: number): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, {
      kind: "add_knowledge_message",
      content: message.content ?? "",
      tokens: tokens ?? Math.max(1, Math.ceil((message.content?.length ?? 0) / 4)),
    })
  }

  /**
   * Spawn an isolated sub-agent via the kernel, run it on the host, and feed the result back.
   * Requires an active parent run (`run()` / `wake()` in progress or paused at milestone).
   */
  async *spawnSubAgent(spec: AgentRunSpec): AsyncIterable<StreamEvent> {
    if (!this.activeKernel || !this.currentSessionId) {
      throw new Error("spawnSubAgent requires an active parent run")
    }
    const parentSessionId = this.currentSessionId
    const runtime = this.activeKernel

    const observations = kernelApply(runtime, this.pendingObservations, {
      kind: "spawn_sub_agent",
      spec: agentRunSpecToKernel(spec),
      parent_session_id: parentSessionId,
    })
    this.nextArchiveStart = await this.appendObservations(parentSessionId, runtime, this.nextArchiveStart)

    const spawned = findSpawnProcessObservation(observations)
    if (!spawned) throw new Error("spawn_sub_agent did not emit agent_process_changed")

    const manifest = spawnObservationToManifest(spawned, spec, parentSessionId)

    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator
    const result = await orchestrator.run({
      parentOpts: this.opts,
      parentSessionId,
      spec,
      manifest,
      sessionLog: this.opts.sessionLog,
      ...(this.opts.subAgentHarness ? { harness: this.opts.subAgentHarness } : {}),
    })

    kernelApply(runtime, this.pendingObservations, {
      kind: "sub_agent_completed",
      result: subAgentResultToKernel(result),
    })
    yield { type: "done", iterations: result.result.turnsUsed, totalTokens: result.result.totalTokensUsed, status: result.result.termination } as DoneEvent
  }

  /**
   * G3: run one workflow node, enforcing its `output_schema` (if any). Without a schema this is a
   * plain `orchestrator.run`. With one, the node's agent is instructed to emit conforming JSON, its
   * output is validated (the supported JSON-Schema subset), and on mismatch the node is re-run once
   * with the validation errors fed back. If it still does not conform, the node is failed with the
   * validation reason — a node that cannot meet its declared output contract starves its dependents,
   * exactly as a denied spawn does.
   */
  private async runWorkflowNode(
    node: WorkflowSpawnInfo,
    parentSessionId: string,
    orchestrator: SubAgentOrchestrator,
    budget?: WorkflowBudget,
    outputs?: Map<string, string>,
    abortSignal?: AbortSignal,
  ): Promise<SubAgentResult> {
    // G2: a reduce node runs no LLM — execute the registered pure function over its dependency
    // outputs and feed the result back as an ordinary completion. Deterministic; no agent burned.
    if (node.reducer) {
      return this.runReduceNode(node, outputs ?? new Map())
    }

    const baseSpec = workflowNodeToSpec(node, parentSessionId)
    const manifest = workflowNodeToManifest(node, parentSessionId)
    // G4: surface the workflow's remaining budget to the node's agent so a coordinator can size its
    // `submit_workflow_nodes` batch to what is available (empty string ⇒ unbounded, no note).
    const budgetNote = workflowBudgetNote(budget)
    const withBudget = (goal: string) => (budgetNote ? `${goal}\n\n${budgetNote}` : goal)
    const mkCtx = (goal: string) => ({
      parentOpts: this.opts,
      parentSessionId,
      spec: { ...baseSpec, goal: withBudget(goal) },
      manifest,
      sessionLog: this.opts.sessionLog,
      // M5 v2.1: this child IS a workflow node — its `start_workflow` flattens to this kernel (the
      // workflow it would author joins the running DAG) rather than bootstrapping a nested pivot.
      isWorkflowNode: true,
      // #2-B-ii: the per-node abort signal the driver fires when the kernel preempts this node.
      ...(abortSignal ? { abortSignal } : {}),
      ...(this.opts.subAgentHarness ? { harness: this.opts.subAgentHarness } : {}),
    })
    const textOf = (r: SubAgentResult): string => {
      const c = r.result.finalMessage?.content
      return typeof c === "string" ? c : c != null ? JSON.stringify(c) : ""
    }
    const withSignal = (r: SubAgentResult, patch: Partial<SubAgentResult["result"]>): SubAgentResult =>
      ({ ...r, result: { ...r.result, ...patch } })

    // A#2 tournament judge: this node compares two entrants' produced outputs rather than running its
    // own goal. Look up both candidates, run a judge over the controller's criterion, and report the
    // winning entrant's agent id as `tournamentWinner` (the kernel advances the bracket with it).
    if (node.judge_match) {
      const out = outputs ?? new Map<string, string>()
      const left = out.get(node.judge_match.left) ?? ""
      const right = out.get(node.judge_match.right) ?? ""
      const result = await orchestrator.run(mkCtx(judgeGoal(baseSpec.goal, left, right)))
      const winner = extractJudgeWinner(textOf(result))
      const winnerId = winner === "right" ? node.judge_match.right : node.judge_match.left
      return withSignal(result, { tournamentWinner: winnerId })
    }

    // A#2 v2 loop iteration: run the increment, then extract a stop signal so the kernel can end the
    // loop early (`loopContinue: false`). No signal ⇒ run to `max_iters`.
    if (node.loop_max_iters != null) {
      const result = await orchestrator.run(mkCtx(`${baseSpec.goal}\n\n${loopInstruction(node.loop_max_iters)}`))
      const cont = extractLoopContinue(textOf(result))
      return cont === undefined ? result : withSignal(result, { loopContinue: cont })
    }

    // A#2 classify: run the classifier, then extract the chosen branch label; the kernel runs that
    // branch and prunes the rest. No recognizable choice ⇒ leave unset (kernel prunes all branches).
    if (node.classify_labels && node.classify_labels.length) {
      const labels = node.classify_labels
      const result = await orchestrator.run(mkCtx(`${baseSpec.goal}\n\n${classifyInstruction(labels)}`))
      const branch = extractClassifyBranch(textOf(result), labels)
      return branch === undefined ? result : withSignal(result, { classifyBranch: branch })
    }

    const schema = node.output_schema
    if (!schema) return orchestrator.run(mkCtx(baseSpec.goal))

    const MAX_ATTEMPTS = 2
    let last: SubAgentResult | undefined
    let lastErrors: string[] = []
    for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
      const goal =
        attempt === 1
          ? `${baseSpec.goal}\n\n${schemaInstruction(schema)}`
          : `${baseSpec.goal}\n\n${schemaRetryInstruction(schema, lastErrors)}`
      const result = await orchestrator.run(mkCtx(goal))
      const content = result.result.finalMessage?.content
      const text = typeof content === "string" ? content : content != null ? JSON.stringify(content) : ""
      const v = validateAgainstSchema(extractJsonValue(text), schema)
      if (v.ok) return result
      last = result
      lastErrors = v.errors
    }

    const reason = `output_schema validation failed after ${MAX_ATTEMPTS} attempts: ${lastErrors.join("; ")}`
    const fallback = last as SubAgentResult
    return {
      ...fallback,
      result: {
        ...fallback.result,
        termination: "error",
        finalMessage: { role: "assistant", content: reason, toolCalls: [] },
      },
    }
  }

  /**
   * G2: execute a deterministic reduce node. Looks up the named reducer (built-ins overlaid with
   * `opts.reducers`), runs it over the node's dependency outputs (gathered from `outputs`), and
   * returns a synthetic completion carrying the reducer's output — no LLM, zero tokens. An unknown
   * reducer or a thrown reducer fails the node (`Error` termination → the kernel starves dependents).
   */
  private runReduceNode(node: WorkflowSpawnInfo, outputs: Map<string, string>): SubAgentResult {
    const ok = (content: string, termination: string): SubAgentResult => ({
      agentId: node.agent_id,
      result: { termination, finalMessage: { role: "assistant", content, toolCalls: [] }, turnsUsed: 0, totalTokensUsed: 0 },
    })
    const reducer = resolveReducer(node.reducer as string, this.opts.reducers)
    if (!reducer) return ok(`unknown reducer "${node.reducer}"`, "error")
    const inputs = (node.input_agent_ids ?? []).map(agentId => ({ agentId, output: outputs.get(agentId) ?? "" }))
    try {
      return ok(reducer(inputs), "completed")
    } catch (err) {
      return ok(`reducer "${node.reducer}" threw: ${err instanceof Error ? err.message : String(err)}`, "error")
    }
  }

  /**
   * W0-ABI: run a declarative workflow DAG. The kernel owns the DAG and gates every node spawn
   * through the syscall trap; this driver runs each kernel-emitted batch of nodes in parallel,
   * feeds their results back, and loops until the kernel reports the workflow complete.
   * Returns the completed / failed node agent-ids.
   */
  async runWorkflow(
    spec: WorkflowSpec,
    opts?: { resumedCompleted?: string[]; resumedSubmissions?: Record<string, unknown>[][] },
  ): Promise<{ completed: string[]; failed: string[]; outputs: Record<string, string> }> {
    if (!this.activeKernel || !this.currentSessionId) {
      throw new Error("runWorkflow requires an active parent run")
    }
    const parentSessionId = this.currentSessionId
    const runtime = this.activeKernel

    const observations = kernelApply(runtime, this.pendingObservations, {
      kind: "load_workflow",
      spec: workflowSpecToKernel(spec),
      parent_session_id: parentSessionId,
      // W0-ABI resume: skip nodes already completed before an interruption.
      ...(opts?.resumedCompleted?.length ? { resumed_completed: opts.resumedCompleted } : {}),
      // R3-1: re-apply recorded runtime submissions so dynamically-appended nodes are reconstructed.
      ...(opts?.resumedSubmissions?.length ? { resumed_submissions: opts.resumedSubmissions } : {}),
    })
    return this.driveWorkflow(observations, parentSessionId, runtime)
  }

  /**
   * M5/G1: bootstrap an **agent-authored** workflow ("the model writes its own harness"). Unlike
   * `runWorkflow` (the host fires the privileged `load_workflow`), this routes the spec through the
   * agent-reachable `Syscall::LoadWorkflow` (the `submit_workflow` event): with no workflow active the
   * kernel **bootstraps** the DAG; if one is already active it **flattens** the spec's nodes onto it
   * (bootstrap-or-flatten — one kernel, one quota, never a workflow stack). Gated by the same
   * `max_workflow_nodes` backstop as runtime submission, so an authored harness can't overgrow the run.
   * The resulting batches are driven by the same shared driver as `runWorkflow`.
   */
  async bootstrapWorkflow(
    spec: WorkflowSpec,
    opts?: { submitterAgentId?: string },
  ): Promise<{ completed: string[]; failed: string[]; outputs: Record<string, string> }> {
    if (!this.activeKernel || !this.currentSessionId) {
      throw new Error("bootstrapWorkflow requires an active parent run")
    }
    const parentSessionId = this.currentSessionId
    const runtime = this.activeKernel
    const observations = kernelApply(
      runtime,
      this.pendingObservations,
      submitWorkflowToKernel(spec, parentSessionId, opts?.submitterAgentId),
    )
    return this.driveWorkflow(observations, parentSessionId, runtime)
  }

  /**
   * M5 v2.1: drive the sub-workflow(s) a top-level agent authored via `start_workflow`. Called at the
   * verified-safe point (right after the tool turn resolved to `call_provider` — kernel in Reason, not
   * suspended). For each authored spec: `bootstrapWorkflow` runs it in THIS kernel (the kernel resumes
   * the agent reason loop on `workflow_completed` — `finish_workflow` sets phase=Reason), then the
   * outcome is injected as a user message so the agent's next turn sees the result. Returns a fresh
   * `call_provider` synthesized from the updated context (the workflow drive consumed its own kernel
   * actions, so we re-render — the same pattern as the reactive-compact retry path).
   */
  private async driveAuthoredWorkflows(
    runtime: KernelRuntimeInstance,
    action: Extract<KernelRunnerAction, { kind: "call_provider" }>,
  ): Promise<Extract<KernelRunnerAction, { kind: "call_provider" }>> {
    const specs = this.pendingAuthoredWorkflows
    this.pendingAuthoredWorkflows = []
    for (const spec of specs) {
      const outcome = await this.bootstrapWorkflow(spec)
      kernelApply(runtime, this.pendingObservations, {
        kind: "add_history_message",
        message: messageToKernelMessage({ role: "user", content: authoredWorkflowOutcomeNote(outcome) }),
      })
    }
    return { kind: "call_provider", context: runtime.render(), tools: action.tools }
  }

  /**
   * #2-B-ii: while a workflow batch is in flight, poll the signal source. A Critical `InterruptNow`
   * routes through the kernel (which, with the root suspended in `SubAgentAwait`, preempts — marks the
   * running nodes `UserAbort`, tears the `WorkflowRun` down, emits `AgentPreempted`); we then abort the
   * matching children's in-flight LLM calls (via their per-node `AbortController` → `interrupt()`).
   * Returns the torn-down workflow's outcome on preemption, else `null`. No-op (null) without a signal
   * source. Non-preempting signals (queue/observe/soft-interrupt) are still applied as they arrive.
   */
  private async monitorWorkflowPreemption(
    runtime: KernelRuntimeInstance,
    controllers: Map<string, AbortController>,
    batchState: { settled: boolean },
  ): Promise<{ completed: string[]; failed: string[] } | null> {
    const source = this.opts.signalSource
    if (!source) return null
    while (!batchState.settled) {
      const sig = await source.nextSignal()
      if (batchState.settled) break
      if (!sig) { await new Promise(resolve => setTimeout(resolve, 5)); continue }
      const obs = kernelApply(runtime, this.pendingObservations, signalToKernelEvent(sig))
      const preempted = obs.find(o => o.kind === "agent_preempted") as { agent_ids?: string[] } | undefined
      if (preempted) {
        for (const id of preempted.agent_ids ?? []) controllers.get(id)?.abort()
        const wc = obs.find(o => o.kind === "workflow_completed") as { completed?: string[]; failed?: string[] } | undefined
        return { completed: wc?.completed ?? [], failed: wc?.failed ?? [] }
      }
    }
    return null
  }

  /**
   * Shared workflow driver for `runWorkflow` (host `load_workflow`) and `bootstrapWorkflow` (agent
   * `submit_workflow`): given the observations from the initial load/bootstrap, run each kernel-emitted
   * batch in parallel, feed completions back (appending any agent-submitted nodes first), and loop
   * until the kernel reports the workflow complete. Returns the completed / failed node agent-ids.
   */
  private async driveWorkflow(
    initial: KernelObservation[],
    parentSessionId: string,
    runtime: KernelRuntimeInstance,
  ): Promise<{ completed: string[]; failed: string[]; outputs: Record<string, string> }> {
    let observations = initial
    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator

    const collectNodes = (obs: typeof observations): WorkflowSpawnInfo[] =>
      (obs.find(o => o.kind === "workflow_batch_spawned") as { nodes?: WorkflowSpawnInfo[] } | undefined)
        ?.nodes ?? []
    // G4: the batch observation also carries the workflow's remaining budget; track the latest so a
    // coordinator node's prompt reflects current headroom when it decides how much to submit.
    const collectBudget = (obs: typeof observations): WorkflowBudget | undefined =>
      (obs.find(o => o.kind === "workflow_batch_spawned") as { budget?: WorkflowBudget } | undefined)?.budget
    const findDone = (obs: typeof observations) =>
      obs.find(o => o.kind === "workflow_completed") as
        | { completed?: string[]; failed?: string[] }
        | undefined

    let done = findDone(observations)
    if (done) return { completed: done.completed ?? [], failed: done.failed ?? [], outputs: {} }
    let nodes = collectNodes(observations)
    let budget = collectBudget(observations)
    // G2: each completed node's output, keyed by agent id — a reduce node reads its dependencies'
    // outputs from here. Deps always complete in an earlier round than the reduce node that needs
    // them (the kernel keeps the reduce node un-ready until its deps finish), so this is populated.
    const outputs = new Map<string, string>()

    for (;;) {
      if (nodes.length === 0) return { completed: [], failed: [], outputs: Object.fromEntries(outputs) } // nothing to run (e.g. all gated)

      // Run the currently-runnable nodes in parallel — each is independent within a round.
      const roundBudget = budget
      // #2-B-ii: per-node abort controllers + a concurrent preemption monitor. While the batch is in
      // flight the monitor polls the signal source; a Critical `InterruptNow` routes through the kernel
      // (which preempts → `AgentPreempted` + tears the workflow down) and we abort the matching child's
      // in-flight LLM call. If the kernel preempted, stop driving and return the torn-down outcome.
      const controllers = new Map(nodes.map(n => [n.agent_id, new AbortController()] as const))
      const batchState = { settled: false }
      const monitor = this.monitorWorkflowPreemption(runtime, controllers, batchState)
      const results = await Promise.all(
        nodes.map(node => this.runWorkflowNode(node, parentSessionId, orchestrator, roundBudget, outputs, controllers.get(node.agent_id)?.signal)),
      )
      batchState.settled = true
      const preempted = await monitor
      if (preempted) return { ...preempted, outputs: Object.fromEntries(outputs) }

      // Feed completions back one at a time. The kernel's run-queue executor may spawn a node's
      // dependents the moment *that* node completes (per-node unblock), so each feed can emit its
      // own `workflow_batch_spawned`; ACCUMULATE them across the round rather than keeping only the
      // last feed's (the old code overwrote `observations` per feed and dropped nodes unblocked by
      // earlier completions — stalling uneven DAGs). Completion and new spawns are mutually
      // exclusive per feed, so a `workflow_completed` only arrives once nothing remains to run.
      const nextNodes: WorkflowSpawnInfo[] = []
      done = undefined
      for (const result of results) {
        // G2: record this node's output so a downstream reduce node can consume it.
        const outContent = result.result.finalMessage?.content
        outputs.set(result.agentId, typeof outContent === "string" ? outContent : outContent != null ? JSON.stringify(outContent) : "")
        // R3-1: if this node's agent submitted more nodes, append them to the parent DAG BEFORE
        // reporting the node's completion — the workflow is still active (the kernel hasn't seen this
        // node finish), so even a submission from the last running node keeps the DAG alive. The
        // appended nodes' `workflow_batch_spawned` is collected into this round like any other.
        if (result.submittedNodes?.length) {
          // G1: stamp the submitting node's agent id so the kernel can coerce a quarantined
          // submitter's nodes to quarantined (no topological privilege escalation).
          const submitEvent = submitWorkflowNodesToKernel(result.submittedNodes, result.agentId)
          const subObs = kernelApply(runtime, this.pendingObservations, submitEvent)
          nextNodes.push(...collectNodes(subObs))
          budget = collectBudget(subObs) ?? budget
          // R3-1: persist the submission (kernel-shape nodes) so resume can re-apply it.
          await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodesSubmittedEvent({
            turn: runtime.turn(),
            nodes: (submitEvent.nodes as Record<string, unknown>[]) ?? [],
          }))
        }
        const obs = kernelApply(runtime, this.pendingObservations, {
          kind: "sub_agent_completed",
          result: subAgentResultToKernel(result),
        })
        nextNodes.push(...collectNodes(obs))
        budget = collectBudget(obs) ?? budget
        const d = findDone(obs)
        if (d) done = d
        // Persist node completion for resume recovery.
        await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodeCompletedEvent({
          turn: runtime.turn(),
          agentId: result.agentId,
          termination: result.result.termination,
        }))
      }
      if (done && nextNodes.length === 0) {
        return { completed: done.completed ?? [], failed: done.failed ?? [], outputs: Object.fromEntries(outputs) }
      }
      nodes = nextNodes
    }
  }

  /**
   * Resume a workflow from the parent session's completed nodes.
   * Reads the session log, extracts completed workflow node agent_ids, and
   * calls runWorkflow with resumedCompleted so the kernel skips those nodes.
   */
  async resumeWorkflow(spec: WorkflowSpec): Promise<{ completed: string[]; failed: string[] }> {
    if (!this.currentSessionId) {
      throw new Error("resumeWorkflow requires an active parent run")
    }
    const events = await this.opts.sessionLog.read(this.currentSessionId)
    const resumedCompleted = recoverCompletedWorkflowNodes(events)
    const resumedSubmissions = recoverSubmittedWorkflowNodes(events)
    return this.runWorkflow(spec, { resumedCompleted, resumedSubmissions })
  }

  interrupt(): void { this.interrupted = true; this.abortController?.abort() }

  async *run(req: {
    sessionId: string
    goal: string
    criteria?: string[]
    /** Multimodal inputs (images / audio) attached to the task as a user message. */
    attachments?: ContentPart[]
    extensions?: Record<string, unknown>
    /** Parent transcript to preload (e.g. sub-agent full context inheritance). */
    inheritEvents?: Array<{ seq: number; event: SessionEvent }>
  }): AsyncIterable<StreamEvent> {
    const prior = req.inheritEvents ?? await this.opts.sessionLog.read(req.sessionId)
    const midRun = isMidRun(prior)
    if (!midRun) {
      await this.opts.sessionLog.append(req.sessionId, {
        kind: "run_started",
        run_id: crypto.randomUUID(),
        goal: req.goal,
        criteria: req.criteria ?? [],
        agent_id: this.opts.agentId,
        system_prompt: this.opts.systemPrompt,
        ...(req.attachments?.length ? { attachments: req.attachments } : {}),
      })
    }
    yield* this.execute(
      req.sessionId,
      req.goal,
      req.criteria ?? [],
      req.extensions,
      prior.length > 0 ? prior : undefined,
      midRun,
      req.attachments,
    )
  }

  async *wake(sessionId: string, extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const events = await this.opts.sessionLog.read(sessionId)
    if (events.some(e => e.event.kind === "run_terminal")) return

    const startEntry = [...events].reverse().find(e => e.event.kind === "run_started")
    if (!startEntry) throw new Error(`No run_started event for session: ${sessionId}`)
    const start = startEntry.event as Extract<SessionEvent, { kind: "run_started" }>

    yield* this.execute(sessionId, start.goal, start.criteria, extensions, events, true, start.attachments)
  }

  async *dream(agentId: string, nowMs = Date.now()): AsyncIterable<StreamEvent> {
    if (!this.opts.dreamStore) throw new Error("dreamStore not configured")
    const kernel = getKernel()

    const sessions = await this.opts.dreamStore.loadSessions(agentId)
    const existingMemories = await this.opts.dreamStore.loadMemories(agentId)
    if (!sessions.length) {
      yield { type: "done", iterations: 0, totalTokens: 0, status: "completed", dreamResult: { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 } } as DoneEvent
      return
    }

    const pipeline = new kernel.IdlePipeline(agentId)
    const action1 = pipeline.feedTrigger(
      sessions.map(s => ({
        sessionId: s.sessionId, agentId: s.agentId,
        messages: s.messages.map(m => ({
          role: m.role, content: m.content, tokenCount: m.tokenCount,
          toolCalls: (m.toolCalls ?? []).map(tc => ({ id: tc.id, name: tc.name, arguments: tc.arguments })),
        })),
        metadata: JSON.stringify(s.metadata ?? null),
        createdAtMs: s.createdAtMs, updatedAtMs: s.updatedAtMs,
      })),
      existingMemories.map(e => ({ text: e.text, score: e.score, metadata: JSON.stringify(e.metadata ?? null) })),
      nowMs,
    )
    if (action1.kind === "noop" || action1.kind === "aborted") {
      yield { type: "done", iterations: 0, totalTokens: 0, status: "completed", dreamResult: { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 } } as DoneEvent
      return
    }
    if (action1.kind !== "synthesize_insights") throw new Error(`unexpected: ${action1.kind}`)

    let synthesisText = ""
    const dreamProvider = this.opts.dreamProvider ?? this.opts.provider
    const providerState = dreamProvider.createRunState?.()
    const synthMsgs = (action1.messages ?? []) as Message[]
    const kernelSystemText = synthMsgs.filter(m => m.role === "system").map(m => m.content).join("\n\n")
    const synthContext = {
      systemText: [kernelSystemText, this.opts.dreamSystemPrompt].filter(Boolean).join("\n\n"),
      turns: synthMsgs.filter(m => m.role !== "system"),
    }
    let totalTokens = 0
    for await (const evt of dreamProvider.stream(synthContext, [], undefined, providerState)) {
      if (evt.type === "text_delta") { synthesisText += (evt as TextDelta).delta; yield evt }
      else if (evt.type === "usage") totalTokens = (evt as { type: string; totalTokens: number }).totalTokens
    }

    const action2 = pipeline.feedSynthesisResult(synthesisText)
    if (action2.kind !== "commit_memories") throw new Error(`unexpected: ${action2.kind}`)
    const cr = action2.curationResult!
    const rr = action2.runResult!

    const dsResult: CurationResult = {
      toAdd: (cr.toAdd ?? []).map((e: MemoryEntry): MemoryEntry => ({
        text: e.text, score: e.score, metadata: tryParseJson(e.metadata as string),
      })),
      toRemoveIndices: (cr.toRemoveIndices ?? []).map(Number),
      stats: {
        insightsProcessed: cr.stats?.insightsProcessed ?? 0,
        duplicatesRemoved: cr.stats?.duplicatesRemoved ?? 0,
        conflictsResolved: cr.stats?.conflictsResolved ?? 0,
        entriesAdded: cr.stats?.entriesAdded ?? 0,
      },
    }
    await this.opts.dreamStore.commit(agentId, dsResult, existingMemories)

    yield {
      type: "done", iterations: 1, totalTokens, status: "completed",
      dreamResult: {
        sessionsProcessed: rr.sessionsProcessed,
        insightsExtracted: rr.insightsExtracted,
        entriesAdded: cr.stats?.entriesAdded ?? 0,
        entriesRemoved: (cr.toRemoveIndices ?? []).length,
      },
    } as DoneEvent
  }

  /** Resolve in-kernel AskUser suspend; returns resume lists and stream events to yield. */
  private async resolveKernelSuspend(
    runtime: KernelRuntimeInstance,
    sessionId: string,
  ): Promise<{ approved: string[]; denied: string[]; events: StreamEvent[] }> {
    const gated = this.pendingObservations.filter(
      (o): o is KernelObservation & { kind: "tool_gated"; call_id: string; tool: string; reason: string } =>
        o.kind === "tool_gated" && typeof o.call_id === "string" && typeof o.tool === "string",
    )
    const approved: string[] = []
    const denied: string[] = []
    const events: StreamEvent[] = []
    const runCtx: RunContext = { onPermissionRequest: this.opts.onPermissionRequest }

    for (const g of gated) {
      const request: PermissionRequestEvent = {
        type: "permission_request",
        callId: g.call_id,
        toolName: g.tool,
        arguments: "{}",
        reason: typeof g.reason === "string" ? g.reason : "",
      }
      events.push(request)
      const decision = await resolvePermissionRequest(request, runCtx)
      events.push({
        type: "permission_resolved",
        callId: g.call_id,
        toolName: g.tool,
        approved: decision.approved,
        responder: decision.responder ?? "host",
        ...(decision.reason ? { reason: decision.reason } : {}),
      } as PermissionResolvedEvent)
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_requested",
        turn: runtime.turn(),
        tool: g.tool,
        arguments: "{}",
        reason: request.reason,
      })
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_resolved",
        turn: runtime.turn(),
        approved: decision.approved,
        responder: decision.responder ?? "host",
      })
      if (decision.approved) {
        approved.push(g.call_id)
      } else {
        denied.push(g.call_id)
        const denyReason = decision.reason ?? "permission denied"
        events.push({
          type: "tool_denied",
          callId: g.call_id,
          toolName: g.tool,
          reason: denyReason,
        } as ToolDeniedEvent)
        events.push({
          type: "tool_result",
          callId: g.call_id,
          name: g.tool,
          content: `permission denied: ${denyReason}`,
          isError: true,
          errorKind: "governance_denied",
        } as ToolResultEvent)
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_denied",
          turn: runtime.turn(),
          call_id: g.call_id,
          tool_name: g.tool,
          reason: denyReason,
        })
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_completed",
          turn: runtime.turn(),
          results: [{
            call_id: g.call_id,
            output: `permission denied: ${denyReason}`,
            is_error: true,
            error_kind: "governance_denied",
          }],
        })
      }
    }

    return { approved, denied, events }
  }

  private async *execute(
    sessionId: string,
    goal: string,
    criteria: string[],
    extensions?: Record<string, unknown>,
    priorEvents?: Array<{ seq: number; event: SessionEvent }>,
    resumeMidRun = false,
    attachments?: ContentPart[],
  ): AsyncIterable<StreamEvent> {
    this.interrupted = false
    this.abortController = new AbortController()
    this.pendingObservations = []
    this.pendingSpoolOutputs.clear()
    this.currentSessionId = sessionId
    if (this.opts.enableDiagnosticsDashboard) {
      this.dashboard = new KernelPrimitivesDashboard(sessionId)
    }
    const kernel = getKernel()
    const ext = { ...this.opts.extensions, ...(extensions ?? {}) }
    const providerState = this.opts.provider.createRunState?.()
    let nextCompressedArchiveStart = nextArchivedSeqStart(priorEvents)

    const providerPolicy = this.opts.provider.runtimePolicy?.() ?? {}
    const effectiveMaxTurns = this.opts.maxTurns ?? providerPolicy.maxTurns ?? 25
    const effectiveTimeoutMs = this.opts.timeoutMs ?? providerPolicy.timeoutMs

    const runtime = new kernel.KernelRuntime({
      maxTokens: this.opts.maxTokens,
      maxTurns: effectiveMaxTurns,
      timeoutMs: effectiveTimeoutMs !== undefined ? BigInt(effectiveTimeoutMs) : undefined,
    })
    this.activeKernel = runtime
    this.nextArchiveStart = nextCompressedArchiveStart

    if (this.opts.tokenizer) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_tokenizer",
        name: this.opts.tokenizer,
      })
    }
    if (this.opts.enablePlanTool !== undefined) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_plan_tool_enabled",
        enabled: this.opts.enablePlanTool,
      })
    }

    kernelApply(runtime, this.pendingObservations, {
      kind: "set_tools",
      tools: this.opts.executionPlane.schemas().map(toolSchemaToKernel),
    })

    if (this.opts.systemPrompt) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "add_system_message",
        content: this.opts.systemPrompt,
        tokens: Math.max(1, Math.ceil(this.opts.systemPrompt.length / 4)),
      })
    }

    if (this.opts.initialMemory) {
      for (const mem of this.opts.initialMemory) {
        kernelApply(runtime, this.pendingObservations, {
          kind: "add_knowledge_message",
          content: mem,
          tokens: Math.max(1, Math.ceil(mem.length / 4)),
        })
      }
    }

    if (this.opts.skillDir) {
      const { scanSkillDir } = await import("../skills/loader.js")
      const metas = await scanSkillDir(this.opts.skillDir)
      // P1-B: pass the full SkillMetadata (incl. `allowedTools`) straight through — re-mapping it
      // field-by-field previously dropped `allowedTools`.
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_available_skills",
        skills: metas.map(m => skillMetadataToKernel(m)),
      })
    }

    // P1-B/D: configure the stable-core tool ids (always exposed under skill gating). Empty/absent
    // ⇒ skills narrow to exactly their declared tools + meta-tools.
    if (this.opts.stableCoreToolIds?.length) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_stable_core_tools",
        tool_ids: this.opts.stableCoreToolIds,
      })
    }

    if (this.opts.dreamStore && this.opts.agentId) {
      kernelApply(runtime, this.pendingObservations, { kind: "set_memory_enabled", enabled: true })
    }
    // Install optional memory policy. Maps the ergonomic camelCase option onto the kernel's
    // snake_case `set_memory_policy` event; omitted fields fall back to kernel defaults.
    if (this.opts.memoryPolicy) {
      const m = this.opts.memoryPolicy
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_memory_policy",
        ...(m.memoryPath !== undefined ? { memory_path: m.memoryPath } : {}),
        ...(m.staleWarningDays !== undefined ? { stale_warning_days: m.staleWarningDays } : {}),
        ...(m.retrievalTopK !== undefined ? { retrieval_top_k: m.retrievalTopK } : {}),
        ...(m.validationEnabled !== undefined ? { validation_enabled: m.validationEnabled } : {}),
        ...(m.maxContentBytes !== undefined ? { max_content_bytes: m.maxContentBytes } : {}),
        ...(m.maxNameLength !== undefined ? { max_name_length: m.maxNameLength } : {}),
      })
    }
    if (this.opts.knowledgeSource) {
      kernelApply(runtime, this.pendingObservations, { kind: "set_knowledge_enabled", enabled: true })
    }

    if (this.opts.milestoneContract) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "load_milestone_contract",
        contract: {
          phases: this.opts.milestoneContract.phases.map(p => ({
            id: p.id,
            criteria: p.criteria ?? [],
            unlocks: p.unlocks ?? [],
            required_evidence: p.requiredEvidence ?? [],
            ...(p.verifier ? { verifier: p.verifier } : {}),
          })),
        },
      })
    }

    const maxBytes = runtime.recoveryContentBytes()

    if (priorEvents && priorEvents.length > 0) {
      const repaired = repairEventsForRecovery(priorEvents, maxBytes)
      seedProviderReplayFromEvents(this.opts.provider, repaired)
      const loadArchive = this.opts.compressionStore
        ? (ref: string) => this.opts.compressionStore!.read(ref)
        : undefined
      const replayed = await replayMessagesAsync(repaired, maxBytes, loadArchive)
      kernelApply(runtime, this.pendingObservations, {
        kind: "preload_history",
        messages: replayed.map(messageToKernelMessage),
      })
      // P1-B B3: rebuild active-skill gating after a wake by re-emitting SkillActivated for each
      // `skill` tool call in the replayed history (active_skills is not snapshotted — graceful).
      // The catalog (set_available_skills) was already fed above, so allowed_tools resolves.
      for (const m of replayed) {
        for (const tc of m.toolCalls ?? []) {
          if (tc.name !== "skill") continue
          try {
            const name = (JSON.parse(tc.arguments || "{}") as { name?: string }).name
            if (name) kernelApply(runtime, this.pendingObservations, { kind: "skill_activated", name })
          } catch { /* malformed skill args — skip */ }
        }
      }
    }

    const sessionStart = Date.now()
    const startPayload: Record<string, unknown> = {
      kind: "start_run",
      task: { goal, criteria },
    }
    // P0-A: lower an explicit `runSpec` and/or the `allowedToolIds` profile to the kernel's
    // `capability_filter`. `allowedToolIds` augments an explicit spec's filter, else synthesizes
    // a minimal top-level spec carrying just the filter (reuses the existing run_spec wire — no
    // new ABI). Unset on both ⇒ no run_spec ⇒ no gating (铁律: no config = old behavior).
    const allowedToolIds = this.opts.allowedToolIds
    const hasProfile = allowedToolIds !== undefined && allowedToolIds.length > 0
    if (this.opts.runSpec || hasProfile) {
      const baseSpec: AgentRunSpec = this.opts.runSpec ?? {
        identity: { agentId: this.opts.agentId ?? "root", sessionId, isSubAgent: false },
        role: "custom",
        goal,
      }
      const spec: AgentRunSpec = hasProfile
        ? { ...baseSpec, capabilityFilter: { ...baseSpec.capabilityFilter, allowedIds: allowedToolIds } }
        : baseSpec
      startPayload.run_spec = agentRunSpecToKernel(spec)
    }
    const osProfile = assertNativeProfile(this.opts.osProfile ?? "native")
    const attentionPolicy = this.opts.attentionPolicy ?? osProfile.attentionPolicy
    const governancePolicy = this.opts.governancePolicy ?? osProfile.governancePolicy

    // Load the declarative governance policy into the kernel before the run starts,
    // so the in-kernel gate enforces deny/veto/rate-limit/param before any tool runs.
    kernelApply(runtime, this.pendingObservations, governancePolicyToKernelEvent(governancePolicy))
    // Enable in-kernel signal routing so the kernel owns disposition + queuing.
    kernelApply(runtime, this.pendingObservations, {
      kind: "set_attention_policy",
      ...(attentionPolicy.maxQueueSize !== undefined
        ? { max_queue_size: attentionPolicy.maxQueueSize }
        : {}),
    })
    // Set optional wall-clock budget override.
    if (this.opts.schedulerBudget) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_scheduler_budget",
        ...(this.opts.schedulerBudget.maxWallMs !== undefined
          ? { max_wall_ms: this.opts.schedulerBudget.maxWallMs }
          : {}),
      })
    }
    // Install optional resource quotas at the syscall trap (M2). Maps the ergonomic camelCase
    // option onto the kernel's snake_case quota shape; the write-rate window is the serde tuple
    // `[maxWrites, windowMs]`. Omitting the option leaves spawn / memory writes unbounded.
    if (this.opts.resourceQuota) {
      const q = this.opts.resourceQuota
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_resource_quota",
        quota: {
          ...(q.maxConcurrentSubagents !== undefined
            ? { max_concurrent_subagents: q.maxConcurrentSubagents }
            : {}),
          ...(q.maxSpawnDepth !== undefined ? { max_spawn_depth: q.maxSpawnDepth } : {}),
          ...(q.memoryWritesPerWindow !== undefined
            ? {
                memory_writes_per_window: [
                  q.memoryWritesPerWindow.maxWrites,
                  q.memoryWritesPerWindow.windowMs,
                ],
              }
            : {}),
        },
      })
    }
    // Multimodal upload: seed the user's attachments (images/audio) as a history
    // message before start_run pushes the "[TASK STATE]" anchor. init_task does not
    // clear history, so order becomes [attachment user msg, "Proceed…"] — both land
    // in the first render. On resume the message is already in the replayed history.
    if (!resumeMidRun && attachments?.length) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "add_history_message",
        message: attachmentsToKernelMessage(attachments),
      })
    }
    // I4: pre-fetch memory into the knowledge partition before the first LLM turn. Skipped on
    // resumes (memory was already on the prior context) and when dreamStore/agentId is absent.
    if (!resumeMidRun && this.opts.preQueryMemory && this.opts.dreamStore && this.opts.agentId) {
      try {
        const queries = await this.opts.preQueryMemory({ goal, runSpec: this.opts.runSpec })
        const entries: Array<{ content: string; tokens?: number; source?: string }> = []
        for (const q of queries ?? []) {
          if (typeof q !== "string" || !q.trim()) continue
          const hits = await this.opts.dreamStore.search(this.opts.agentId, q, 5)
          for (const hit of hits) {
            entries.push({ content: `[memory score=${hit.score.toFixed(3)}] ${hit.text}`, source: "memory" })
          }
        }
        if (entries.length > 0) {
          kernelApply(runtime, this.pendingObservations, { kind: "page_in", entries })
        }
      } catch { /* errs-open — a faulty pre-fetch never breaks the run */ }
    }

    let action: KernelRunnerAction = resumeMidRun
      ? kernelAction(runtime, this.pendingObservations, { kind: "resume" })
      : kernelAction(runtime, this.pendingObservations, startPayload)
    let hasAttemptedReactiveCompact = false
    // P0-C: the skill loaded and in effect going into the current turn (updated when the model's
    // `skill` tool call resolves). Drives the per-turn `activeSkill` metric → dwell measurement.
    let activeSkill: string | undefined

    // I0b: wrap the main loop so any uncaught kernel exception (typically a NAPI
    // Status::InvalidArg from a malformed input — e.g. RuntimeSignal.source with a wrong shape,
    // or an unrecognized event kind) is observable rather than silently propagating out of the
    // async generator. Without this wrap the runner emits no `run_terminal` event, so downstream
    // observability (session log, bench mechanism hooks) can't distinguish "the kernel rejected
    // an input" from "the run is still in progress."
    try {
    while (!runtime.isTerminal()) {
      // Page-in must run before appendObservations drains pending kernel observations.
      if (action.kind === "execute_tool") {
        await this.applyKernelPageIn(runtime, sessionId)
      }
      nextCompressedArchiveStart = await this.appendObservations(
        sessionId,
        runtime,
        nextCompressedArchiveStart,
      )
      this.nextArchiveStart = nextCompressedArchiveStart
      if (this.interrupted) {
        action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
        break
      }

      if (this.opts.signalSource) {
        const sig = await this.opts.signalSource.nextSignal()
        if (sig) {
          // Kernel-routed: the kernel decides disposition (dedup/queue/interrupt) and emits
          // `signal_disposed`. An actionable disposition yields a new action to adopt; queued/observed/
          // ignored yields none (kernel buffers).
          const sigAction = kernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(sig))
          if (sigAction) action = sigAction
          // I0a: a Critical-urgency signal carries user_abort intent. The kernel disposes it as
          // InterruptNow (forces a Reason turn) but does NOT call abortController.abort() unless
          // sub-agents are suspended — so the no-sub-agent path (e.g. the signal-injection bench
          // scenario) wouldn't otherwise set `this.interrupted`, and the eventual run_terminal would
          // report `reason: "error"` indistinguishable from a crash. Mark it here so the final
          // classification in the run_terminal emit picks `user_abort`.
          if (sig.urgency === "critical") this.interrupted = true
        }
      }
      if (runtime.isTerminal()) break

      if (action.kind === "call_provider") {
        // M5 v2.1: top-level auto-pivot at the safe point. If the agent authored sub-workflow(s) via
        // `start_workflow`, drive each in THIS kernel now (the kernel is in Reason / `call_provider`,
        // NOT suspended — driving mid-suspend would clobber the single-slot suspend state), inject the
        // outcome into context, and re-render. Loop-top placement (vs only after `tool_results`) catches
        // EVERY path to `call_provider` — including resuming after an approval gate — so a queued spec
        // is never stranded. Drains the queue; fires once per authored batch.
        if (this.pendingAuthoredWorkflows.length > 0) {
          action = await this.driveAuthoredWorkflows(runtime, action)
        }
        const finalToolCalls: ToolCall[] = []
        let finalText = ""
        // I5: governance schema-level pre-filter. When a declarative GovernancePolicy is loaded
        // and `surfaceDeniedInSystem !== false`, drop denied tools from the schema BEFORE the
        // model sees them — the model can't plan a call it doesn't know about, so the rollback
        // overhead disappears. The list of denied names is appended to systemKnowledge so the
        // model knows not to plan around them.
        let context = action.context
        let tools = action.tools
        if (this.opts.governancePolicy && this.opts.governancePolicy.surfaceDeniedInSystem !== false) {
          const { allowed, denied } = governanceFilterSchema(tools, this.opts.governancePolicy)
          if (denied.length > 0) {
            tools = allowed
            const note = `[governance] the following tools are denied for this run and will fail if called: ${denied.join(", ")}.`
            context = {
              ...context,
              systemKnowledge: context.systemKnowledge
                ? `${context.systemKnowledge}\n\n${note}`
                : note,
            }
          }
        }
        let turnTokens = 0
        let turnInputTokens = 0
        let turnOutputTokens = 0
        let turnCacheReadTokens = 0
        let turnCacheCreationTokens = 0
        let turnCacheReadBySlot: { system?: number; tools?: number; messages?: number } | undefined
        let shouldRetry = false

        const abortSignal = this.abortController?.signal
        try {
          for await (const evt of this.opts.provider.stream(context, tools, Object.keys(ext).length ? ext : undefined, providerState, abortSignal)) {
            // #2-B-ii: a preempting `interrupt()` fires `abortController` — stop consuming the live
            // stream immediately (providers that forward `signal` also abort the socket; the rest at
            // least stop here at the next event). The loop-top `interrupted` check then ends the run.
            if (abortSignal?.aborted) break
            if (evt.type === "usage") {
              const usageEvt = evt as { type: string; totalTokens: number; inputTokens?: number; outputTokens?: number; cacheReadInputTokens?: number; cacheCreationInputTokens?: number; cacheReadInputTokensBySlot?: { system?: number; tools?: number; messages?: number } }
              turnTokens = usageEvt.totalTokens
              turnInputTokens = usageEvt.inputTokens ?? 0
              turnOutputTokens = usageEvt.outputTokens ?? 0
              // P0-C: capture the prompt-cache split for the tool-gating hit-rate baseline.
              turnCacheReadTokens = usageEvt.cacheReadInputTokens ?? 0
              turnCacheCreationTokens = usageEvt.cacheCreationInputTokens ?? 0
              // I1: per-slot attribution forwarded into TurnMetrics. Undefined when the provider
              // doesn't honor cache_control (OpenAI-family auto-cache).
              turnCacheReadBySlot = usageEvt.cacheReadInputTokensBySlot
              continue
            }
            yield evt
            if (evt.type === "text_delta") finalText += (evt as TextDelta).delta
            else if (evt.type === "tool_call") {
              const tc = evt as ToolCallEvent
              finalToolCalls.push({ id: tc.id, name: tc.name, arguments: JSON.stringify(tc.arguments) })
            }
          }
        } catch (err) {
          // #2-B-ii: an aborted in-flight request surfaces as an AbortError — treat it as an interrupt
          // (the loop-top `interrupted` check converts it to a clean `timeout`/UserAbort), not a crash.
          if (abortSignal?.aborted) { this.interrupted = true }
          const errMsg = String(err).toLowerCase()
          if (
            (errMsg.includes("413") || errMsg.includes("too long") || errMsg.includes("context length exceeded") || errMsg.includes("context_length_exceeded")) &&
            !hasAttemptedReactiveCompact
          ) {
            hasAttemptedReactiveCompact = true
            if (forceCompact(runtime, this.pendingObservations)) {
              nextCompressedArchiveStart = await this.appendObservations(
                sessionId,
                runtime,
                nextCompressedArchiveStart,
              )
              shouldRetry = true
            }
          }
          if (!shouldRetry) {
            yield { type: "error", message: String(err) } as ErrorEvent
            action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
            break
          }
        }

        // #2-B-ii: stream aborted (preempt/interrupt) via the break path (provider yielded no error) —
        // end the turn now with a timeout so the kernel terminates the run, rather than feeding the
        // partial assistant output as a normal turn.
        if (abortSignal?.aborted) {
          action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
          break
        }

        if (shouldRetry) {
          action = {
            kind: "call_provider",
            context: runtime.render(),
            tools,
          }
          continue
        }

        const assistantMessage: Message = {
          role: "assistant",
          content: finalText,
          toolCalls: finalToolCalls,
          tokenCount: turnOutputTokens || turnTokens || undefined,
        }
        const providerEvent: Record<string, unknown> = {
          kind: "provider_result",
          message: messageToKernelMessage(assistantMessage),
          ...(turnInputTokens > 0 ? { observed_input_tokens: turnInputTokens } : {}),
          ...(turnOutputTokens > 0 ? { observed_output_tokens: turnOutputTokens } : {}),
          now_ms: Date.now(),
        }
        let nextAction = kernelMaybeAction(runtime, this.pendingObservations, providerEvent)
        if (!nextAction && this.pendingObservations.some(o => o.kind === "suspended")) {
          const resolved = await this.resolveKernelSuspend(runtime, sessionId)
          for (const evt of resolved.events) yield evt
          nextAction = kernelAction(runtime, this.pendingObservations, {
            kind: "resume",
            approved_calls: resolved.approved,
            denied_calls: resolved.denied,
          })
        }
        action = nextAction ?? kernelAction(runtime, this.pendingObservations, providerEvent)
        const providerReplay = peekProviderReplay(this.opts.provider, finalText, finalToolCalls)
        await this.opts.sessionLog.append(sessionId, buildLlmCompletedEvent({
          turn: runtime.turn(),
          content: finalText,
          tokenCount: turnOutputTokens || turnTokens || undefined,
          toolCalls: finalToolCalls,
          providerReplay,
        }))

        // P0-C: emit per-turn tool-gating telemetry. `activeSkill` reflects the skill in effect
        // GOING INTO this turn; a `skill` call here only takes effect next turn, so emit first, then
        // advance. Wrapped so a faulty sink can never break the run (pure observation).
        if (this.opts.onTurnMetrics) {
          try {
            this.opts.onTurnMetrics({
              turn: runtime.turn(),
              toolsExposed: tools.length,
              toolsCalled: finalToolCalls.length,
              activeSkill,
              inputTokens: turnInputTokens,
              cacheReadTokens: turnCacheReadTokens,
              cacheCreationTokens: turnCacheCreationTokens,
              ...(turnCacheReadBySlot ? { cacheReadTokensBySlot: turnCacheReadBySlot } : {}),
            })
          } catch { /* metrics must never break the run */ }
        }
        const skillCall = finalToolCalls.find(c => c.name === "skill")
        if (skillCall) {
          try {
            const name = (JSON.parse(skillCall.arguments || "{}") as { name?: string }).name
            if (name) activeSkill = name
          } catch { /* malformed skill args — leave activeSkill unchanged */ }
        }

      } else if (action.kind === "execute_tool") {
        const allCalls: ToolCall[] = action.calls
        await this.opts.sessionLog.append(sessionId, { kind: "tool_requested", turn: runtime.turn(), calls: allCalls })

        const runCtx: RunContext = {
          agentId: this.opts.agentId,
          skillDir: this.opts.skillDir,
          dreamStore: this.opts.dreamStore,
          knowledgeSource: this.opts.knowledgeSource,
          onToolSuspend: this.opts.onToolSuspend,
          onPermissionRequest: this.opts.onPermissionRequest,
          resultSpool: this.opts.resultSpool ?? new LargeResultSpool(),
        }

        const toolResults: ToolResult[] = []
        const normalCalls = allCalls.filter(
          c => c.name !== "update_plan" && c.name !== "submit_workflow_nodes" && c.name !== "start_workflow",
        )
        const planCalls = allCalls.filter(c => c.name === "update_plan")
        // M5 v1: `start_workflow` (author a sub-workflow) flattens to the same append path as
        // `submit_workflow_nodes` — a `WorkflowSpec` is a node batch. (v2 adds top-level bootstrap.)
        const submitCalls = allCalls.filter(c => c.name === "submit_workflow_nodes" || c.name === "start_workflow")

        for (const call of planCalls) {
          const update = parseUpdatePlanArgs(call.arguments)
          kernelApply(runtime, this.pendingObservations, {
            kind: "update_task",
            update: taskUpdateToKernel(update),
          })
          const result = { callId: call.id, output: "success", isError: false }
          toolResults.push(result)
          yield { type: "tool_result", callId: call.id, content: "success", isError: false } as ToolResultEvent
        }

        // R3-1: `submit_workflow_nodes` cannot be applied to this runner's kernel — when this runner
        // is a workflow node, the workflow lives in the *parent* kernel. Surface the requested nodes
        // as a stream event; the orchestrator collects them onto the node's result and `runWorkflow`
        // sends `submit_workflow_nodes` to the parent kernel. (When not a workflow node, the event is
        // simply unconsumed — a no-op.)
        for (const call of submitCalls) {
          // M5 v2.1: a TOP-LEVEL agent authoring a whole sub-workflow via `start_workflow` — record the
          // full spec and AUTO-PIVOT once this tool turn resolves (the loop drives it in this kernel and
          // injects the outcome). A workflow-NODE's `start_workflow` (and every `submit_workflow_nodes`)
          // instead FLATTENS: the batch is surfaced for the parent `runWorkflow` to append.
          if (call.name === "start_workflow" && !this.opts.isWorkflowNode) {
            const spec = parseStartWorkflowSpec(call.arguments)
            if (spec) {
              this.pendingAuthoredWorkflows.push(spec)
              const out = "workflow authored; executing now"
              toolResults.push({ callId: call.id, output: out, isError: false })
              yield { type: "tool_result", callId: call.id, content: out, isError: false } as ToolResultEvent
              continue
            }
          }
          // `start_workflow` wraps the batch as `{ spec: { nodes } }`; `submit_workflow_nodes` is `{ nodes }`.
          const nodes = call.name === "start_workflow"
            ? parseStartWorkflowArgs(call.arguments)
            : parseSubmitWorkflowNodesArgs(call.arguments)
          yield { type: "workflow_nodes_submitted", nodes } as WorkflowNodesSubmittedEvent
          const result = { callId: call.id, output: "submitted", isError: false }
          toolResults.push(result)
          yield { type: "tool_result", callId: call.id, content: "submitted", isError: false } as ToolResultEvent
        }

        if (normalCalls.length > 0) {
          for await (const evt of this.opts.executionPlane.executeAll(normalCalls, runCtx)) {
            yield evt
            if (evt.type === "tool_result") {
              const tre = evt as ToolResultEvent
              toolResults.push({
                callId: tre.callId,
                output: tre.content,
                isError: tre.isError,
                isFatal: tre.isFatal,
                errorKind: tre.errorKind,
              })
            } else if (evt.type === "tool_argument_repaired") {
              const tare = evt as ToolArgumentRepairedEvent
              await this.opts.sessionLog.append(sessionId, {
                kind: "tool_argument_repaired",
                turn: runtime.turn(),
                tool: tare.name,
                original_arguments: tare.originalArguments,
                repaired_arguments: tare.repairedArguments,
              })
            } else if (evt.type === "tool_denied") {
              const tde = evt as ToolDeniedEvent
              await this.opts.sessionLog.append(sessionId, {
                kind: "tool_denied",
                turn: runtime.turn(),
                call_id: tde.callId,
                tool_name: tde.toolName,
                reason: tde.reason,
              })
            } else if (evt.type === "permission_request") {
              const pre = evt as PermissionRequestEvent
              const turn = runtime.turn()
              await this.opts.sessionLog.append(sessionId, {
                kind: "permission_requested",
                turn,
                tool: pre.toolName,
                arguments: pre.arguments,
                reason: pre.reason,
              })
            } else if (evt.type === "permission_resolved") {
              const resolved = evt as PermissionResolvedEvent
              const turn = runtime.turn()
              await this.opts.sessionLog.append(sessionId, {
                kind: "permission_resolved",
                turn,
                approved: resolved.approved,
                responder: resolved.responder,
              })
            }
          }
          const names = normalCalls.map(c => c.name).join(", ")
          kernelApply(runtime, this.pendingObservations, {
            kind: "update_task",
            update: taskUpdateToKernel({ progress: `Executed tools: ${names}` }),
          })
        }

        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_completed",
          turn: runtime.turn(),
          results: toolResults.map(r => ({
            call_id: r.callId,
            output: r.output,
            is_error: r.isError,
            token_count: r.tokenCount,
          })),
        })
        for (const call of normalCalls) {
          const result = toolResults.find(r => r.callId === call.id)
          if (result) {
            this.pendingSpoolOutputs.set(call.id, { tool: call.name, output: result.output })
          }
        }
        // P1-B B3: a `skill` call that resolved successfully activates that skill in the kernel, so
        // the next `call_provider` narrows the toolset to its declared tools. Fed before `tool_results`
        // (which computes the next action). Errs-open: a failed/missing skill load doesn't activate.
        for (const call of allCalls) {
          if (call.name !== "skill") continue
          const res = toolResults.find(r => r.callId === call.id)
          if (!res || res.isError) continue
          try {
            const name = (JSON.parse(call.arguments || "{}") as { name?: string }).name
            if (name) kernelApply(runtime, this.pendingObservations, { kind: "skill_activated", name })
          } catch { /* malformed skill args — skip activation */ }
        }
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "tool_results",
          results: toolResults.map(toolResultToKernel),
        })

      } else if (action.kind === "evaluate_milestone") {
        const milestonePolicy = this.opts.milestonePolicy ?? "require_verifier"
        if (milestonePolicy === "auto_pass") {
          action = kernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            result: milestoneCheckResultToKernel(milestoneCheckPass(action.phaseId)),
          })
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
          )
        } else if (this.opts.onMilestoneEvaluate) {
          const check = await this.opts.onMilestoneEvaluate({
            phaseId: action.phaseId,
            criteria: action.criteria,
            requiredEvidence: action.requiredEvidence,
          })
          action = kernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            result: milestoneCheckResultToKernel(check),
          })
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
          )
        } else {
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
          )
          const turnsUsed = Math.max(1, runtime.turn())
          await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
            reason: "milestone_pending",
            turnsUsed,
            totalTokens: 0,
          }))
          yield { type: "done", iterations: turnsUsed, totalTokens: 0, status: "milestone_pending" } as DoneEvent
          this.activeKernel = null
          this.currentSessionId = null
          return
        }

      } else if (action.kind === "done") {
        break
      }
    }
    } catch (err) {
      // I0b: kernel rejection (or any other thrown error inside the loop) reaches us here.
      // Classify by NAPI status code or message pattern — `invalid_arg` for surface-shape rejects,
      // `error` for everything else — then emit run_terminal so observability sees a clean end.
      // The yield-error path mirrors what the in-flight provider-stream catch does.
      const errMsg = err instanceof Error ? err.message : String(err)
      const code = (err as { code?: string }).code
      const isInvalidArg = code === "InvalidArg" ||
        errMsg.toLowerCase().includes("invalidarg") ||
        errMsg.toLowerCase().includes("invalid argument")
      const reason = isInvalidArg ? "invalid_arg" : "error"
      yield { type: "error", message: errMsg } as ErrorEvent
      try {
        await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
          reason,
          turnsUsed: runtime.turn() || 0,
          totalTokens: 0,
        }))
      } catch { /* session log failure must not mask the original error */ }
      yield { type: "done", iterations: runtime.turn() || 0, totalTokens: 0, status: reason } as DoneEvent
      this.activeKernel = null
      this.currentSessionId = null
      this.dashboard = null
      return
    }

    const result = action.kind === "done" ? action.result : undefined
    // I0a: when the loop exits without a clean kernel-done — typically because a hard interrupt
    // aborted the in-flight LLM stream and the catch path sent `timeout` (which the kernel handles
    // by injecting a rollback note and continuing, not by terminating) — preserve the preempt
    // intent in the run_terminal reason. Without this, every interrupt-curtailed run reports
    // `reason: "error"` and the bench / observability layer can't distinguish preemption from a
    // genuine crash. Mirrors WASM/Python/Rust.
    const status = result?.termination ?? (this.interrupted ? "user_abort" : "error")
    const turnsUsed = result ? Math.max(1, result.turnsUsed) : runtime.turn() || 0
    const totalTokens = result?.totalTokensUsed ?? 0

    nextCompressedArchiveStart = await this.appendObservations(
      sessionId,
      runtime,
      nextCompressedArchiveStart,
    )
    await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
      reason: status,
      turnsUsed,
      totalTokens,
    }))

    if (this.opts.dreamStore && this.opts.agentId) {
      const newMsgs = runtime.drainNewMessages().map(m => ({
        role: m.role,
        content: m.content,
        contentParts: m.contentParts,
        tokenCount: m.tokenCount,
        toolCalls: m.toolCalls?.length ? m.toolCalls : undefined,
      }))
      if (newMsgs.length > 0) {
        try {
          await this.opts.dreamStore.saveSession({
            sessionId: crypto.randomUUID(),
            agentId: this.opts.agentId,
            messages: newMsgs,
            metadata: null,
            createdAtMs: sessionStart,
            updatedAtMs: Date.now(),
          } as SessionData)
        } catch { /* non-fatal */ }
      }
    }

    yield { type: "done", iterations: turnsUsed, totalTokens, status } as DoneEvent
    this.activeKernel = null
    this.currentSessionId = null
    this.dashboard = null
  }

  private async appendObservations(
    sessionId: string,
    runtime: KernelRuntimeInstance,
    nextArchiveStart: number,
  ): Promise<number> {
    const turn = runtime.turn()
    const preservedRefs = runtime.preservedRefs()
    const observations = this.pendingObservations.splice(0)
    for (let obs of observations) {
      if (obs.kind === "page_in_requested") continue

      let archiveRef: string | undefined
      let spoolRef: string | undefined
      if (obs.kind === "compressed") {
        const archived = obs.archived
        if (this.opts.compressionStore && archived && archived.length > 0) {
          try {
            const pathRef = await this.opts.compressionStore.write(sessionId, nextArchiveStart, archived)
            if (pathRef) archiveRef = pathRef
          } catch {
            // non-fatal
          }
        }
      }

      if (obs.kind === "page_out" && obs.archived) {
        this.localPageOutCache.push(...(obs.archived as Message[]))
      }

      if (obs.kind === "large_result_spooled") {
        const pending = this.pendingSpoolOutputs.get(obs.call_id ?? "")
        if (pending) {
          const spool = this.opts.resultSpool ?? new LargeResultSpool()
          try {
            spoolRef = await spool.persistOutput(obs.call_id ?? "", pending.output)
          } catch {
            // non-fatal: preview remains in kernel context; full output still in tool_completed log
          }
          if (!obs.tool && pending.tool) {
            obs = { ...obs, tool: pending.tool }
          }
          this.pendingSpoolOutputs.delete(obs.call_id ?? "")
        }
      }

      const latest =
        obs.kind === "compressed" ? await this.opts.sessionLog.latestSeq(sessionId) : undefined
      const event = kernelObservationToSessionEvent(obs, turn, {
        nextArchiveStart,
        latestSeq: latest,
        archiveRef,
        spoolRef,
        preservedRefs,
        compressionAction,
      })
      if (!event) continue

      const compressedSeq = await this.opts.sessionLog.append(sessionId, event)
      if (event.kind === "compressed") {
        nextArchiveStart = compressedSeq + 1
        const archived = obs.kind === "compressed" ? obs.archived : undefined
        if (this.opts.asyncSummarizer && archived && archived.length > 0) {
          void this.upgradeCompressedSummary(
            sessionId,
            compressedSeq,
            archived as Message[],
            compressionAction(obs.action) ?? "auto_compact",
          )
        }
      }
      if (
        obs.kind === "page_out"
        && obs.tier_hint === "semantic"
        && Array.isArray(obs.archived)
        && obs.archived.length > 0
      ) {
        void this.archiveSemanticPageOut(obs.archived as Message[], compressionAction(obs.action))
      }
    }
    return nextArchiveStart
  }

  private async archiveSemanticPageOut(archived: Message[], action?: string): Promise<void> {
    if (!this.opts.dreamStore || !this.opts.agentId) return
    try {
      const summary = this.opts.dreamSummarizer
        ? await this.opts.dreamSummarizer.summarize(archived, { action })
        : await summarizeForLongTermMemory(
          this.opts.dreamProvider ?? this.opts.provider,
          archived,
          this.opts.dreamSystemPrompt,
        )
      const existing = await this.opts.dreamStore.loadMemories(this.opts.agentId)
      await this.opts.dreamStore.commit(this.opts.agentId, {
        toAdd: [{ text: summary, score: 1.0, metadata: { source: "semantic_page_out", action } }],
        toRemoveIndices: [],
        stats: {
          insightsProcessed: 1,
          duplicatesRemoved: 0,
          conflictsResolved: 0,
          entriesAdded: 1,
        },
      }, existing)
    } catch {
      // non-fatal: in-context compression summary remains; long-term layer is best-effort
    }
  }

  private async upgradeCompressedSummary(
    sessionId: string,
    compressedSeq: number,
    archived: Message[],
    action: string,
  ): Promise<void> {
    try {
      const summary = await this.opts.asyncSummarizer!.summarize(archived, action)
      await this.opts.sessionLog.append(sessionId, {
        kind: "summary_upgraded",
        compressed_seq: compressedSeq,
        summary,
      })
    } catch {
      // non-fatal: rule-based summary stays in place
    }
  }
}

function isMidRun(events: Array<{ seq: number; event: SessionEvent }>): boolean {
  return events.length > 0 && !events.some(e => e.event.kind === "run_terminal")
}

/**
 * Build a kernel `add_history_message` payload from user attachments: a `user`
 * message whose content is the multimodal parts in the kernel's serde shape
 * (`Content::Parts`; image `media_type`, not `mediaType`). Lets a caller upload
 * images/audio with the task — the message lands in history before the first render.
 */
function attachmentsToKernelMessage(parts: ContentPart[]): Record<string, unknown> {
  const content = parts.map(p => {
    if (p.type === "image") {
      return {
        type: "image",
        ...(p.url ? { url: p.url } : {}),
        ...(p.data ? { data: p.data } : {}),
        ...(p.mediaType ? { media_type: p.mediaType } : {}),
        ...(p.detail ? { detail: p.detail } : {}),
      }
    }
    if (p.type === "audio") return { type: "audio", data: p.data, media_type: p.mediaType }
    if (p.type === "text") return { type: "text", text: p.text }
    return { type: "text", text: "" }
  })
  return { role: "user", content }
}

function compressionAction(action?: string): Extract<SessionEvent, { kind: "compressed" }>["action"] {
  if (
    action === "snip_compact" ||
    action === "micro_compact" ||
    action === "context_collapse" ||
    action === "auto_compact"
  ) {
    return action
  }
  return undefined
}

async function summarizeForLongTermMemory(
  provider: LLMProvider,
  archived: Message[],
  systemPrompt?: string,
): Promise<string> {
  const transcript = archived
    .map(m => `${m.role}: ${m.content}`)
    .join("\n")
  const context = {
    systemText: [
      systemPrompt,
      "Summarize the following conversation for long-term memory. Preserve key facts, decisions, and open questions.",
    ].filter(Boolean).join("\n\n"),
    turns: [{ role: "user" as const, content: transcript, toolCalls: [] }],
  }
  let text = ""
  const state = provider.createRunState?.()
  for await (const evt of provider.stream(context, [], undefined, state)) {
    if (evt.type === "text_delta" && "delta" in evt) text += evt.delta
  }
  return text.trim() || transcript.slice(0, 2000)
}

export function replayMessages(events: Array<{ seq: number; event: SessionEvent }>, maxBytes?: number): Message[] {
  // Build upgraded-summary index: compressed_seq -> upgraded summary
  const upgradedSummaries = new Map<number, string>()
  for (const { event: e } of events) {
    if (e.kind === "summary_upgraded") upgradedSummaries.set(e.compressed_seq, e.summary)
  }

  const messages: Message[] = []
  for (const { seq, event: e } of events) {
    if (e.kind === "run_started") {
      const userText = e.criteria.length
        ? `${e.goal}\n\nCriteria:\n${e.criteria.map((c, i) => `${i + 1}. ${c}`).join("\n")}`
        : e.goal
      messages.push({
        role: "user",
        content: userText,
        toolCalls: [],
        tokenCount: Math.max(1, Math.ceil(userText.length / 4)),
      })
    } else if (e.kind === "compressed") {
      const summary = upgradedSummaries.get(seq) ?? e.summary
      if (summary) {
        const systemText = `[Compressed context: turn ${e.turn}]\n${summary}`
        messages.push({
          role: "system",
          content: systemText,
          toolCalls: [],
          tokenCount: Math.max(1, Math.ceil(systemText.length / 4)),
        })
      }
    } else if (e.kind === "llm_completed") {
      messages.push({
        role: "assistant",
        content: sanitizeReplayText(e.content, maxBytes),
        toolCalls: e.tool_calls ?? [],
        tokenCount: e.token_count,
      })
    } else if (e.kind === "tool_completed") {
      for (const r of e.results) {
        messages.push({
          role: "tool",
          content: "",
          toolCalls: [],
          contentParts: [{ type: "tool_result", callId: r.call_id, output: sanitizeReplayText(r.output, maxBytes), isError: r.is_error ?? false }],
          tokenCount: r.token_count,
        })
      }
    } else if (e.kind === "rollbacked") {
      const len = e.checkpoint_history_len ?? 0
      if (messages.length > len) {
        messages.length = len
      }
    }
  }
  return messages
}

export async function replayMessagesAsync(
  events: Array<{ seq: number; event: SessionEvent }>,
  maxBytes?: number,
  loadArchive?: (archiveRef: string) => Promise<Message[]>,
): Promise<Message[]> {
  // Build upgraded-summary index: compressed_seq -> upgraded summary
  const upgradedSummaries = new Map<number, string>()
  for (const { event: e } of events) {
    if (e.kind === "summary_upgraded") upgradedSummaries.set(e.compressed_seq, e.summary)
  }

  const messages: Message[] = []
  for (const { seq, event: e } of events) {
    if (e.kind === "run_started") {
      const userText = e.criteria.length
        ? `${e.goal}\n\nCriteria:\n${e.criteria.map((c, i) => `${i + 1}. ${c}`).join("\n")}`
        : e.goal
      messages.push({
        role: "user",
        content: userText,
        toolCalls: [],
        tokenCount: Math.max(1, Math.ceil(userText.length / 4)),
      })
    } else if (e.kind === "compressed") {
      let loadedSuccessfully = false
      if (e.archive_ref && loadArchive) {
        try {
          const archivedMsgs = await loadArchive(e.archive_ref)
          for (const msg of archivedMsgs) {
            messages.push({
              role: msg.role,
              content: sanitizeReplayText(msg.content, maxBytes),
              toolCalls: msg.toolCalls ?? [],
              tokenCount: msg.tokenCount,
            })
          }
          loadedSuccessfully = true
        } catch (err) {
          // Loader failed (e.g. MissingArchive). We degrade and fallback.
        }
      }

      if (!loadedSuccessfully) {
        const summary = upgradedSummaries.get(seq) ?? e.summary
        if (summary) {
          const systemText = `[Compressed context: turn ${e.turn}]\n${summary}`
          messages.push({
            role: "system",
            content: systemText,
            toolCalls: [],
            tokenCount: Math.max(1, Math.ceil(systemText.length / 4)),
          })
        }
      }
    } else if (e.kind === "llm_completed") {
      messages.push({
        role: "assistant",
        content: sanitizeReplayText(e.content, maxBytes),
        toolCalls: e.tool_calls ?? [],
        tokenCount: e.token_count,
      })
    } else if (e.kind === "tool_completed") {
      for (const r of e.results) {
        messages.push({
          role: "tool",
          content: "",
          toolCalls: [],
          contentParts: [{ type: "tool_result", callId: r.call_id, output: sanitizeReplayText(r.output, maxBytes), isError: r.is_error ?? false }],
          tokenCount: r.token_count,
        })
      }
    } else if (e.kind === "rollbacked") {
      const len = e.checkpoint_history_len ?? 0
      if (messages.length > len) {
        messages.length = len
      }
    }
  }
  return messages
}

function nextArchivedSeqStart(events?: Array<{ seq: number; event: SessionEvent }>): number {
  let next = 0
  for (const { event } of events ?? []) {
    if (event.kind === "compressed") next = Math.max(next, event.archived_seq_range[1] + 1)
  }
  return next
}

function tryParseJson(s: string): unknown {
  try { return JSON.parse(s) } catch { return null }
}

/** Collect all text_delta events from a run into a single string. */
export async function collectText(stream: AsyncIterable<StreamEvent>): Promise<string> {
  let text = ""
  for await (const evt of stream) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
  }
  return text
}

function parseUpdatePlanArgs(argsStr: string): import("../types.js").TaskUpdate {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error
  }
  return {
    plan: parsed.plan as string[] | undefined,
    currentStep: parsed.currentStep !== undefined ? Number(parsed.currentStep) : parsed.current_step !== undefined ? Number(parsed.current_step) : undefined,
    progress: parsed.progress as string | undefined,
    scratchpad: parsed.scratchpad as string | undefined,
    blockedOn: parsed.blockedOn !== undefined
      ? parsed.blockedOn as string[]
      : parsed.blocked_on as string[] | undefined,
  }
}

/** R3-1: parse the `submit_workflow_nodes` tool arguments (`{ nodes: WorkflowNodeSpec[] }`). Node
 *  shapes are trusted structurally here; the kernel validates them (dep range, quarantine, quota) on
 *  append. A malformed payload yields no nodes rather than throwing. */
function parseSubmitWorkflowNodesArgs(argsStr: string): WorkflowNodeSpec[] {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error → no nodes submitted.
  }
  return Array.isArray(parsed.nodes) ? (parsed.nodes as WorkflowNodeSpec[]) : []
}

/** M5 v1: parse the `start_workflow` tool arguments (`{ spec: { nodes: WorkflowNodeSpec[] } }`) into
 *  the spec's node batch — flattened onto the running workflow via the same append path. A malformed
 *  payload yields no nodes rather than throwing. */
function parseStartWorkflowArgs(argsStr: string): WorkflowNodeSpec[] {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error → no nodes.
  }
  const spec = parsed.spec as { nodes?: unknown } | undefined
  return Array.isArray(spec?.nodes) ? (spec!.nodes as WorkflowNodeSpec[]) : []
}

/** M5 v2.1: parse the full `WorkflowSpec` from a top-level `start_workflow` call, for auto-pivot drive
 *  (vs `parseStartWorkflowArgs`, which returns only the node batch for the flatten path). Returns
 *  `undefined` on a malformed / empty payload so the caller falls back to the flatten path. */
function parseStartWorkflowSpec(argsStr: string): WorkflowSpec | undefined {
  try {
    const parsed = JSON.parse(argsStr) as { spec?: { nodes?: unknown } }
    if (Array.isArray(parsed.spec?.nodes) && parsed.spec!.nodes.length > 0) {
      return { nodes: parsed.spec!.nodes as WorkflowNodeSpec[] }
    }
  } catch {
    // Ignore parse error → undefined (fall back to flatten).
  }
  return undefined
}

/** M5 v2.1: render an authored-workflow outcome into a user-message note injected back into the
 *  agent's context, so the agent's next turn continues with the sub-workflow's results in view. */
function authoredWorkflowOutcomeNote(outcome: {
  completed: string[]
  failed: string[]
  outputs: Record<string, string>
}): string {
  const lines = [
    `[authored workflow result] ${outcome.completed.length} node(s) completed` +
      (outcome.failed.length ? `, ${outcome.failed.length} failed` : "") + ".",
  ]
  for (const id of outcome.completed) {
    const out = outcome.outputs[id]
    if (out) lines.push(`- ${id}: ${out.length > 500 ? out.slice(0, 500) + "…" : out}`)
  }
  return lines.join("\n")
}

/** Lower a host `RuntimeSignal` to the kernel's snake_case `signal` input event. Shared by the main
 *  loop's per-turn poll and #2-B-ii's workflow-batch preemption monitor (so the two never drift). */
function signalToKernelEvent(sig: RuntimeSignal): Record<string, unknown> {
  return {
    kind: "signal",
    signal: {
      id: crypto.randomUUID(),
      source: sig.source ?? "custom",
      signal_type: sig.signalType ?? "event",
      urgency: sig.urgency ?? "normal",
      summary: String((sig.payload as Record<string, unknown>)?.goal ?? sig.kind ?? "signal"),
      payload: sig.payload ?? {},
      ...(sig.dedupeKey ? { dedupe_key: sig.dedupeKey } : {}),
      timestamp_ms: Date.now(),
    },
  }
}
