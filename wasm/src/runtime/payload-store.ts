export interface PayloadStorageDriver {
  write(key: string, content: string): Promise<void> | void
  read(key: string): Promise<string | undefined> | string | undefined
  delete(key: string): Promise<void> | void
  list(): Promise<string[]> | string[]
  mtime?(key: string): Promise<number> | number
}

export class MemoryPayloadDriver implements PayloadStorageDriver {
  private readonly values = new Map<string, { content: string; mtime: number }>()

  async write(key: string, content: string): Promise<void> {
    this.values.set(key, { content, mtime: Date.now() })
  }

  async read(key: string): Promise<string | undefined> {
    return this.values.get(key)?.content
  }

  async delete(key: string): Promise<void> {
    this.values.delete(key)
  }

  async list(): Promise<string[]> {
    return [...this.values.keys()]
  }

  async mtime(key: string): Promise<number> {
    return this.values.get(key)?.mtime ?? 0
  }
}

export interface PayloadStoreConfig {
  driver?: PayloadStorageDriver
  maxAgeMs?: number
}

async function sha256(value: string): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))
  return [...new Uint8Array(digest)]
    .map(byte => byte.toString(16).padStart(2, "0"))
    .join("")
}

/** Driver-backed storage for canonical opaque payload locators. */
export class PayloadStore {
  private readonly driver: PayloadStorageDriver
  private readonly maxAgeMs?: number
  private readonly activeWrites = new Map<string, Promise<void>>()

  constructor(config: PayloadStoreConfig = {}) {
    this.driver = config.driver ?? new MemoryPayloadDriver()
    this.maxAgeMs = config.maxAgeMs
  }

  private async key(sessionId: string, payloadRef: string): Promise<string> {
    return `payload/${await sha256(`${sessionId}\u0000${payloadRef}`)}`
  }

  async persistPayload(sessionId: string, payloadRef: string, content: string): Promise<void> {
    const key = await this.key(sessionId, payloadRef)
    let write = this.activeWrites.get(key)
    if (!write) {
      write = Promise.resolve(this.driver.write(key, content)).finally(() => {
        this.activeWrites.delete(key)
      })
      this.activeWrites.set(key, write)
    }
    await write
  }

  async loadPayload(sessionId: string, payloadRef: string): Promise<string | undefined> {
    return await this.driver.read(await this.key(sessionId, payloadRef))
  }

  async cleanup(maxAgeMs = this.maxAgeMs ?? 7 * 24 * 60 * 60 * 1000): Promise<number> {
    if (!this.driver.mtime) return 0
    const now = Date.now()
    let removed = 0
    for (const key of await this.driver.list()) {
      if (!key.startsWith("payload/")) continue
      if (now - await this.driver.mtime(key) > maxAgeMs) {
        await this.driver.delete(key)
        removed += 1
      }
    }
    return removed
  }
}
