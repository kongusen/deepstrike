export interface ToolResult {
  callId: string
  tool: string
  output: string
  isError?: boolean
}

export interface SpooledToolResult {
  originalOutput: string
  preview: string
  spoolRef: string
  wasSpooled: boolean
}

export interface SpoolConfig {
  spoolThresholdBytes: number
  previewTokens: number
  totalMessageLimitBytes: number
  maxAgeMs?: number
  driver?: SpoolStorageDriver
}

export interface SpoolStorageDriver {
  write(key: string, content: string): Promise<void> | void
  read(key: string): Promise<string> | string
  delete(key: string): Promise<void> | void
  list(): Promise<string[]> | string[]
  mtime?(key: string): Promise<number> | number
}

export class MemorySpoolDriver implements SpoolStorageDriver {
  private cache = new Map<string, { content: string; mtime: number }>()

  async write(key: string, content: string): Promise<void> {
    this.cache.set(key, { content, mtime: Date.now() })
  }

  async read(key: string): Promise<string> {
    const val = this.cache.get(key)
    if (!val) throw new Error(`Spooled result not found: ${key}`)
    return val.content
  }

  async delete(key: string): Promise<void> {
    this.cache.delete(key)
  }

  async list(): Promise<string[]> {
    return Array.from(this.cache.keys())
  }

  async mtime(key: string): Promise<number> {
    return this.cache.get(key)?.mtime ?? 0
  }
}

export const DEFAULT_SPOOL_CONFIG: SpoolConfig = {
  spoolThresholdBytes: 50 * 1024, // 50KB
  previewTokens: 500, // ~2KB
  totalMessageLimitBytes: 200 * 1024, // 200KB
}

function simpleHash(content: string): string {
  let hash = 5381
  for (let i = 0; i < content.length; i++) {
    hash = (hash * 33) ^ content.charCodeAt(i)
  }
  return (hash >>> 0).toString(16)
}

export class LargeResultSpool {
  private config: SpoolConfig
  private driver: SpoolStorageDriver
  private activeWrites = new Map<string, Promise<string>>()

  constructor(config: Partial<SpoolConfig> = {}) {
    this.config = { ...DEFAULT_SPOOL_CONFIG, ...config }
    this.driver = this.config.driver ?? new MemorySpoolDriver()
  }

  private needsSpool(result: ToolResult): boolean {
    return result.output.length > this.config.spoolThresholdBytes
  }

  private getSpoolKey(hash: string): string {
    return `.spool/${hash}.txt`
  }

  private async writeToDriver(content: string, hash: string): Promise<string> {
    const key = this.getSpoolKey(hash)
    let promise = this.activeWrites.get(key)
    if (!promise) {
      promise = (async () => {
        try {
          await this.driver.write(key, content)
          return key
        } finally {
          this.activeWrites.delete(key)
        }
      })()
      this.activeWrites.set(key, promise)
    }
    return promise
  }

  private generatePreview(content: string): string {
    const previewTokens = Math.min(this.config.previewTokens, content.length / 4)
    const preview = content.substring(0, previewTokens)
    const omitted = content.length - previewTokens

    return `[tool_result_spooled]
size: ${content.length} bytes
preview: first ${previewTokens} chars
omitted: ${omitted} chars
[full content available via Read tool]
`
  }

  async processToolResult(result: ToolResult): Promise<SpooledToolResult> {
    if (!this.needsSpool(result)) {
      return {
        originalOutput: result.output,
        preview: result.output,
        spoolRef: "",
        wasSpooled: false,
      }
    }

    const hash = simpleHash(result.output)
    const spoolRef = await this.writeToDriver(result.output, hash)
    const preview = this.generatePreview(result.output)

    return {
      originalOutput: result.output,
      preview,
      spoolRef,
      wasSpooled: true,
    }
  }

  private callKey(sessionId: string, callId: string): string {
    // Session-scoped and hashed: the driver's key space is shared across sessions and outlives
    // runs, while vendor call ids can be index-style ("call_0") and repeat — an unscoped key
    // lets read_result in one session fetch another session's spooled output. Hashing also keeps
    // untrusted call-id characters out of driver keys (parity with Node/Python).
    return simpleHash(`${sessionId}\u0000${callId}`)
  }

  async persistOutput(sessionId: string, callId: string, content: string): Promise<string> {
    const hash = simpleHash(content)
    const key = `.spool/${this.callKey(sessionId, callId)}-${hash.slice(0, 16)}.txt`

    let promise = this.activeWrites.get(key)
    if (!promise) {
      promise = (async () => {
        try {
          await this.driver.write(key, content)
          return key
        } finally {
          this.activeWrites.delete(key)
        }
      })()
      this.activeWrites.set(key, promise)
    }
    return promise
  }

  async readSpooledResult(spoolRef: string): Promise<string> {
    return this.driver.read(spoolRef)
  }

  /**
   * O7: locate a spooled output by the tool call's id (the `read_result` meta-tool only knows
   * `call_id`, not the content-hashed key `persistOutput` chose). Scans the driver's key list for
   * the hashed session-scoped call-key prefix; returns `undefined` if nothing was ever
   * spooled for that call.
   */
  async findByCallId(sessionId: string, callId: string): Promise<string | undefined> {
    let keys: string[]
    try {
      keys = await this.driver.list()
    } catch {
      return undefined
    }
    const prefix = `.spool/${this.callKey(sessionId, callId)}-`
    const match = keys.find(k => k.startsWith(prefix) && k.endsWith('.txt'))
    if (!match) return undefined
    try {
      return await this.driver.read(match)
    } catch {
      return undefined
    }
  }

  async cleanup(maxAgeMs?: number): Promise<number> {
    const limit = maxAgeMs ?? this.config.maxAgeMs ?? 7 * 24 * 60 * 60 * 1000
    try {
      const keys = await this.driver.list()
      let count = 0
      const now = Date.now()
      for (const key of keys) {
        if (this.driver.mtime) {
          const mtime = await this.driver.mtime(key)
          if (now - mtime > limit) {
            await this.driver.delete(key)
            count++
          }
        }
      }
      return count
    } catch {
      return 0
    }
  }
}
