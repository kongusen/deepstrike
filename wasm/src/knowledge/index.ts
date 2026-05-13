export interface KnowledgeSource {
  retrieve(goal: string, topK?: number): Promise<string[]>
  /** One-time warmup called before the first run (load index, open connection, etc.). */
  init(): Promise<void>
}
