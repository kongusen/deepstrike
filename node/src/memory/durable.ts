import type { Memory, MemoryQuery, MemoryRecord, MemoryScope, MemorySearchOptions, MemoryStore } from "./protocols.js"

/**
 * Binds the host storage protocol to the public Memory contract for one agent and namespace.
 * This is deliberately an SDK adapter: it does not lower memory mutations into the Kernel.
 */
export class DurableMemory implements Memory {
  readonly namespace: string

  constructor(
    private readonly store: MemoryStore,
    private readonly agentId: string,
    private readonly scope: MemoryScope,
  ) {
    this.namespace = scope.namespace
  }

  async search(query: string, options: MemorySearchOptions = {}): Promise<MemoryRecord[]> {
    const request: MemoryQuery = {
      scope: this.scope,
      query,
      top_k: options.topK ?? 5,
      kinds: options.kinds ?? [],
      ...(options.minScore === undefined ? {} : { min_score: options.minScore }),
    }
    return (await this.store.search(this.agentId, request)).map(hit => hit.record)
  }

  async get(id: string): Promise<MemoryRecord | null> {
    const record = await this.store.get(this.agentId, id)
    return record !== null && sameScope(record.scope, this.scope) ? record : null
  }

  async put(record: MemoryRecord): Promise<void> {
    if (!sameScope(record.scope, this.scope)) {
      throw new Error("memory record scope must match the bound Memory scope")
    }
    await this.store.put(this.agentId, record)
  }

  async delete(id: string): Promise<void> {
    const record = await this.get(id)
    if (record !== null) await this.store.delete(this.agentId, id)
  }
}

function sameScope(left: MemoryScope, right: MemoryScope): boolean {
  return left.tenant_id === right.tenant_id && left.namespace === right.namespace
}
