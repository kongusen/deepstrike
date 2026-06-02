/**
 * Long-term memory agent implementation (Phase 7).
 *
 * Provides two main capabilities:
 * 1. Extract memories from conversation transcripts (后台代理)
 * 2. Select relevant memories for current context (LLM选择器)
 *
 * Design principles:
 * - Kernel defines memory types and validation rules
 * - SDK performs I/O and selection
 * - LLM (Sonnet) acts as selector, not vector similarity
 */

import type { SessionData, MemoryEntry, DreamStore } from "./protocols.js"

/**
 * Memory metadata (matches kernel MemoryMetadata structure).
 */
export interface MemoryMetadata {
  name: string
  description: string
  kind?: MemoryKind
  created_at: number
  updated_at: number
  session_id?: string

  // Heuristic inference fields
  user_role?: string
  expertise_level?: string
  preference_rule?: string
  approved_pattern?: string
  project_phase?: string
  relative_date?: string
  external_url?: string
  ticket_ref?: string
}

/**
 * Memory kind (4 types, mirroring Claude Code).
 */
export type MemoryKind = "user" | "feedback" | "project" | "reference"

/**
 * Memory write request (SDK → kernel).
 */
export interface MemoryWriteRequest {
  metadata: MemoryMetadata
  content: string
}

/**
 * Memory query request (kernel → SDK).
 */
export interface MemoryQuery {
  current_context: string
  active_tools: string[]
  already_surfaced: string[]
  top_k: number
}

/**
 * Memory retrieval response (SDK → kernel).
 */
export interface MemoryRetrieval {
  selected_memory_ids: string[]
  selection_rationale: string
}

/**
 * Memory index entry (from MEMORY.md).
 */
export interface MemoryIndexEntry {
  name: string
  description: string
  kind?: MemoryKind
  file: string
  updated_at: number
}

/**
 * Extract memories from a completed session.
 *
 * This is typically called by a background agent after a session completes.
 * It analyzes the conversation and generates new memory write requests.
 *
 * @param sessionData - The completed session data
 * @param existingMemories - Existing memories to avoid duplicates
 * @returns Array of memory write requests
 */
export async function extractMemories(
  sessionData: SessionData,
  existingMemories: MemoryEntry[],
): Promise<MemoryWriteRequest[]> {
  // In a real implementation, this would:
  // 1. Use an LLM to analyze the session transcript
  // 2. Identify new insights, preferences, and context
  // 3. Generate memory write requests with proper classification
  // 4. Check for duplicates against existing memories

  // For now, return a stub implementation
  return []
}

/**
 * Select relevant memories for the current context.
 *
 * This uses an LLM (Sonnet) as a selector, not vector similarity.
 * The process:
 * 1. Read memory index (name + description for each memory)
 * 2. Filter out already_surfaced and recentTools
 * 3. Send the filtered list to LLM with current context
 * 4. LLM returns top-5 most relevant memory IDs
 *
 * @param query - Memory query from kernel
 * @param memoryIndex - Memory index entries
 * @param model - Optional model name (default: claude-sonnet-4-20250514)
 * @returns Memory retrieval result
 */
export async function selectMemories(
  query: MemoryQuery,
  memoryIndex: MemoryIndexEntry[],
  model: string = "claude-sonnet-4-20250514",
): Promise<MemoryRetrieval> {
  // 1. Filter out already surfaced and tool-related memories
  const filterOut = new Set([
    ...query.already_surfaced,
    ...query.active_tools,
  ])

  const candidates = memoryIndex.filter(
    (entry) => !filterOut.has(entry.name) && !isToolMemory(entry),
  )

  // 2. If no candidates, return empty
  if (candidates.length === 0) {
    return {
      selected_memory_ids: [],
      selection_rationale: "No candidates after filtering",
    }
  }

  // 3. In a real implementation, this would:
  // - Construct a prompt with current context and memory descriptions
  // - Call the LLM API
  // - Parse the response to extract selected memory IDs
  // For now, return a stub implementation

  return {
    selected_memory_ids: candidates.slice(0, query.top_k).map((c) => c.name),
    selection_rationale: "Stub implementation",
  }
}

/**
 * Check if a memory is tool-related (should be filtered from recentTools).
 */
function isToolMemory(entry: MemoryIndexEntry): boolean {
  const toolKeywords = ["usage", "how to use", "example", "syntax", "api"]
  const lowerDesc = entry.description.toLowerCase()
  const lowerName = entry.name.toLowerCase()

  // Filter out usage docs, but keep warnings/caveats
  const isUsage = toolKeywords.some((kw) => lowerDesc.includes(kw) || lowerName.includes(kw))
  const isWarning = lowerDesc.includes("warning") || lowerDesc.includes("caveat") || lowerDesc.includes("bug")

  return isUsage && !isWarning
}

/**
 * Validate memory before writing (kernel-side validation mirror).
 *
 * This SDK-side validation provides early feedback before sending to kernel.
 */
export function validateMemory(request: MemoryWriteRequest): { valid: boolean; error?: string } {
  // Check required fields
  if (!request.metadata.name || request.metadata.name.trim().length === 0) {
    return { valid: false, error: "Missing required field: name" }
  }
  if (!request.metadata.description || request.metadata.description.trim().length === 0) {
    return { valid: false, error: "Missing required field: description" }
  }

  // Check name length
  if (request.metadata.name.length > 100) {
    return { valid: false, error: `Name too long: ${request.metadata.name.length} chars (limit: 100)` }
  }

  // Check content size
  if (request.content.length > 10_000) {
    return { valid: false, error: `Content too large: ${request.content.length} bytes (limit: 10000)` }
  }

  // Check forbidden patterns
  const forbiddenPatterns = [
    { pattern: "代码模式:", reason: "应从代码推，不应存储" },
    { pattern: "文件路径:", reason: "应从git推，不应存储" },
    { pattern: "架构:", reason: "应从实际代码推" },
    { pattern: "git历史:", reason: "git log是权威" },
    { pattern: "CLAUDE.md:", reason: "已在文档中" },
    { pattern: "TODO:", reason: "临时任务不应进记忆" },
  ]

  for (const { pattern, reason } of forbiddenPatterns) {
    if (request.content.includes(pattern)) {
      return { valid: false, error: `Forbidden pattern '${pattern}': ${reason}` }
    }
  }

  return { valid: true }
}

/**
 * Infer memory kind from metadata (mirrors kernel logic).
 */
export function inferMemoryKind(metadata: MemoryMetadata): MemoryKind {
  if (metadata.user_role || metadata.expertise_level) {
    return "user"
  }
  if (metadata.preference_rule || metadata.approved_pattern) {
    return "feedback"
  }
  if (metadata.project_phase || metadata.relative_date) {
    return "project"
  }
  if (metadata.external_url || metadata.ticket_ref) {
    return "reference"
  }
  // Default: feedback (most common)
  return "feedback"
}

/** Build a memory index from DreamStore entries for {@link selectMemories}. */
export function memoriesToIndex(entries: MemoryEntry[]): MemoryIndexEntry[] {
  return entries.map(entry => {
    const meta = (entry.metadata ?? {}) as Record<string, unknown>
    return {
      name: String(meta.name ?? entry.text.slice(0, 40)),
      description: String(meta.description ?? entry.text.slice(0, 120)),
      kind: meta.kind as MemoryKind | undefined,
      file: String(meta.file ?? ""),
      updated_at: Number(meta.updated_at ?? 0),
    }
  })
}
