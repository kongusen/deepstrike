import type { KnowledgeSource } from "./source.js"

/** Public knowledge descriptor, distinct from the executable `KnowledgeSource` retriever. */
export type KnowledgeSourceRef =
  | { kind: "file"; path: string }
  | { kind: "directory"; path: string }
  | { kind: "text"; content: string }
  | { kind: "url"; url: string }
  | { kind: "vector"; retriever: KnowledgeSource }
  | { kind: "custom"; [key: string]: unknown }

export interface Knowledge {
  id?: string
  name?: string
  source: KnowledgeSourceRef
  description?: string
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}
