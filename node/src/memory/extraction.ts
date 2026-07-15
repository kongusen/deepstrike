import type { LLMProvider, Message } from "../types.js"
import type { MemoryKind, MemoryRecord, MemoryScope, SessionData } from "./protocols.js"

const KINDS = new Set<MemoryKind>(["user", "feedback", "project", "reference"])

export async function extractSessionMemories(
  provider: LLMProvider,
  session: SessionData,
  scope: MemoryScope,
  systemPrompt?: string,
): Promise<MemoryRecord[]> {
  const transcript = session.messages
    .map(message => `[${message.role.toUpperCase()}] ${message.content}`)
    .join("\n")
    .slice(0, 8_000)
  const context = {
    systemText: [
      systemPrompt,
      "Extract durable, reusable facts from this completed session. Return only JSON; do not include transient progress or guesses.",
    ].filter(Boolean).join("\n\n"),
    turns: [{
      role: "user" as const,
      content: `${transcript}\n\nReturn {"memories":[{"name":"stable-kebab-key","kind":"user|feedback|project|reference","content":"fact","description":"why durable","confidence":0.0,"links":[],"pinned":false,"ttl_days":null,"evidence_refs":[]}]} with at most 10 items. Return {"memories":[]} when nothing is durable.`,
      toolCalls: [],
    } satisfies Message],
  }
  let output = ""
  const state = provider.createRunState?.()
  for await (const event of provider.stream(context, [], undefined, state)) {
    if (event.type === "text_delta" && "delta" in event) output += event.delta
  }
  return parseExtractedMemories(output, session, scope)
}

export function parseExtractedMemories(
  output: string,
  session: SessionData,
  scope: MemoryScope,
): MemoryRecord[] {
  let value: unknown
  try {
    value = JSON.parse(output.trim().replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/, ""))
  } catch {
    return []
  }
  if (!value || typeof value !== "object" || !Array.isArray((value as { memories?: unknown }).memories)) return []
  const now = session.updatedAtMs
  const records: MemoryRecord[] = []
  for (const raw of (value as { memories: unknown[] }).memories.slice(0, 10)) {
    if (!raw || typeof raw !== "object") continue
    const draft = raw as Record<string, unknown>
    const name = typeof draft.name === "string" ? draft.name.trim() : ""
    const kind = typeof draft.kind === "string" && KINDS.has(draft.kind as MemoryKind)
      ? draft.kind as MemoryKind
      : undefined
    const content = typeof draft.content === "string" ? draft.content.trim() : ""
    if (!name || !kind || !content) continue
    const confidence = typeof draft.confidence === "number" && Number.isFinite(draft.confidence)
      ? Math.max(0, Math.min(1, draft.confidence))
      : 0.5
    records.push({
      record_id: `${scope.tenant_id}:${scope.namespace}:${kind}:${name}`,
      scope,
      name,
      kind,
      content,
      description: typeof draft.description === "string" ? draft.description.trim() : "",
      provenance: {
        session_id: session.sessionId,
        author: "extraction",
        trust: "untrusted",
        evidence_refs: Array.isArray(draft.evidence_refs)
          ? draft.evidence_refs.filter((ref): ref is string => typeof ref === "string")
          : [],
      },
      created_at: now,
      updated_at: now,
      recall_count: 0,
      confidence,
      links: Array.isArray(draft.links) ? draft.links.filter((link): link is string => typeof link === "string") : [],
      pinned: draft.pinned === true,
      ...(typeof draft.ttl_days === "number" && Number.isInteger(draft.ttl_days) && draft.ttl_days > 0
        ? { ttl_days: draft.ttl_days }
        : {}),
    })
  }
  return records
}
