export interface KnowledgeSource {
  retrieve(goal: string, topK?: number): Promise<string[]>
  /** One-time warmup called before the first run (load index, open connection, etc.). */
  init(): Promise<void>
}

export interface TextKnowledgeDocument {
  id?: string
  name?: string
  content: string
}

/** Deterministic local retrieval for inline text knowledge; external sources keep explicit bindings. */
export function createTextKnowledgeSource(documents: TextKnowledgeDocument[]): KnowledgeSource {
  const prepared = documents.filter(document => document.content.trim()).map((document, index) => ({
    ...document,
    key: document.id ?? document.name ?? `text-${index}`,
    terms: new Set(document.content.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? []),
  }))
  return {
    async init(): Promise<void> {},
    async retrieve(goal: string, topK = 5): Promise<string[]> {
      const queryTerms = new Set(goal.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [])
      return prepared
        .map(document => ({ document, score: [...queryTerms].reduce((score, term) => score + (document.terms.has(term) ? 1 : 0), 0) }))
        .sort((left, right) => right.score - left.score || left.document.key.localeCompare(right.document.key))
        .filter(item => item.score > 0 || queryTerms.size === 0)
        .slice(0, Math.max(0, topK))
        .map(({ document }) => document.name ? `[Knowledge: ${document.name}]\n${document.content}` : document.content)
    },
  }
}
