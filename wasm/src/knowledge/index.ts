export interface KnowledgeSource {
  retrieve(goal: string, topK?: number): Promise<string[]>
}
