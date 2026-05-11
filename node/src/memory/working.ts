export class WorkingMemory {
  private store: Map<string, unknown> = new Map()

  set(key: string, value: unknown): void { this.store.set(key, value) }
  get<T = unknown>(key: string, defaultValue?: T): T | undefined { return (this.store.get(key) ?? defaultValue) as T }
  delete(key: string): void { this.store.delete(key) }
  clear(): void { this.store.clear() }
  has(key: string): boolean { return this.store.has(key) }
}
