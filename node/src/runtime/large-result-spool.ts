/**
 * Large result spool (Layer 1 of 5-layer compression pyramid).
 *
 * When a single tool result exceeds 50KB, write the full content to disk
 * and keep only a 2KB preview in the message. Zero API overhead.
 *
 * Design principles:
 * - Kernel defines policy (thresholds)
 * - SDK performs I/O (disk write/read)
 * - Model can retrieve full content via Read tool when needed
 */

import * as crypto from 'crypto'
import * as fs from 'fs/promises'
import * as path from 'path'

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

/**
 * Large result spool configuration (mirrors kernel ContextConfig).
 */
export interface SpoolConfig {
  /** Single result size threshold (bytes) */
  spoolThresholdBytes: number
  /** Preview token count (~2KB) */
  previewTokens: number
  /** Total message limit (bytes) */
  totalMessageLimitBytes: number
  /** Custom spool directory path */
  spoolDir?: string
  /** Maximum age of spooled files (default 7 days) */
  maxAgeMs?: number
}

export const DEFAULT_SPOOL_CONFIG: SpoolConfig = {
  spoolThresholdBytes: 50 * 1024, // 50KB
  previewTokens: 500, // ~2KB
  totalMessageLimitBytes: 200 * 1024, // 200KB
}

/**
 * Large result spool manager.
 */
export class LargeResultSpool {
  private config: SpoolConfig
  private spoolDir: string
  private activeWrites = new Map<string, Promise<string>>()

  constructor(config: Partial<SpoolConfig> = {}) {
    this.config = { ...DEFAULT_SPOOL_CONFIG, ...config }
    this.spoolDir = config.spoolDir ?? '.spool'
  }

  /**
   * Check if a tool result needs spooling.
   */
  private needsSpool(result: ToolResult): boolean {
    return result.output.length > this.config.spoolThresholdBytes
  }

  /**
   * Hash content for spool reference.
   */
  private hashContent(content: string): string {
    return crypto.createHash('sha256').update(content).digest('hex')
  }

  /**
   * Get spool file path for a hash.
   */
  private getSpoolPath(hash: string): string {
    return path.join(this.spoolDir, `${hash}.txt`)
  }

  private callKey(sessionId: string, callId: string): string {
    // Session-scoped: the spool dir is shared across sessions and outlives runs, while vendor
    // call ids can be index-style ("call_0") and repeat — an unscoped key lets read_result in
    // one session fetch another session's spooled output.
    return this.hashContent(`${sessionId}\u0000${callId}`).slice(0, 32)
  }

  private async atomicWrite(spoolPath: string, content: string): Promise<void> {
    const tempPath = `${spoolPath}.${process.pid}.${crypto.randomUUID()}.tmp`
    let handle: fs.FileHandle | undefined
    try {
      handle = await fs.open(tempPath, 'wx')
      await handle.writeFile(content, 'utf-8')
      await handle.sync()
      await handle.close()
      handle = undefined
      await fs.rename(tempPath, spoolPath)
    } finally {
      await handle?.close().catch(() => undefined)
      await fs.unlink(tempPath).catch(() => undefined)
    }
  }

  /**
   * Write large result to disk.
   */
  private async writeToDisk(
    content: string,
    hash: string
  ): Promise<string> {
    const spoolPath = this.getSpoolPath(hash)

    let promise = this.activeWrites.get(spoolPath)
    if (!promise) {
      promise = (async () => {
        try {
          await fs.mkdir(this.spoolDir, { recursive: true })
          await this.atomicWrite(spoolPath, content)
          return spoolPath
        } finally {
          this.activeWrites.delete(spoolPath)
        }
      })()
      this.activeWrites.set(spoolPath, promise)
    }
    return promise
  }

  /**
   * Generate preview for a tool result.
   */
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

  /**
   * Process a tool result: spool if large, return spooled result.
   */
  async processToolResult(result: ToolResult): Promise<SpooledToolResult> {
    if (!this.needsSpool(result)) {
      return {
        originalOutput: result.output,
        preview: result.output,
        spoolRef: '',
        wasSpooled: false
      }
    }

    // Hash the content
    const hash = this.hashContent(result.output)

    // Write to disk
    const spoolRef = await this.writeToDisk(result.output, hash)

    // Generate preview
    const preview = this.generatePreview(result.output)

    return {
      originalOutput: result.output,
      preview,
      spoolRef,
      wasSpooled: true
    }
  }

  /**
   * Persist a kernel-spooled tool output to disk. Returns the on-disk path ref.
   */
  async persistOutput(sessionId: string, callId: string, content: string): Promise<string> {
    const hash = this.hashContent(content)
    const spoolPath = this.getSpoolPath(`${this.callKey(sessionId, callId)}-${hash.slice(0, 16)}`)

    let promise = this.activeWrites.get(spoolPath)
    if (!promise) {
      promise = (async () => {
        try {
          await fs.mkdir(this.spoolDir, { recursive: true })
          await this.atomicWrite(spoolPath, content)
          return spoolPath
        } finally {
          this.activeWrites.delete(spoolPath)
        }
      })()
      this.activeWrites.set(spoolPath, promise)
    }
    return promise
  }

  /**
   * Read a spooled result back from disk.
   */
  async readSpooledResult(spoolRef: string): Promise<string> {
    try {
      const content = await fs.readFile(spoolRef, 'utf-8')
      return content
    } catch (error) {
      throw new Error(`Failed to read spooled result: ${error}`)
    }
  }

  /**
   * O7: locate a spooled output by the tool call's id (the `read_result` meta-tool only knows
   * `call_id`, not the content-hashed file name `persistOutput` chose). Scans the spool directory
   * for the hashed call-key prefix; returns `undefined` if nothing was ever spooled
   * for that call (e.g. it never actually exceeded the threshold, or the spool dir was cleaned up).
   */
  async findByCallId(sessionId: string, callId: string): Promise<string | undefined> {
    let files: string[]
    try {
      files = await fs.readdir(this.spoolDir)
    } catch {
      return undefined
    }
    const prefix = `${this.callKey(sessionId, callId)}-`
    const match = files.find(f => f.startsWith(prefix) && f.endsWith('.txt'))
    if (!match) return undefined
    try {
      return await fs.readFile(path.join(this.spoolDir, match), 'utf-8')
    } catch {
      return undefined
    }
  }

  /**
   * Clean up old spool files (optional maintenance).
   */
  async cleanup(maxAgeMs?: number): Promise<number> {
    const limit = maxAgeMs ?? this.config.maxAgeMs ?? 7 * 24 * 60 * 60 * 1000
    try {
      const files = await fs.readdir(this.spoolDir)
      let count = 0
      const now = Date.now()
      for (const file of files) {
        const filePath = path.join(this.spoolDir, file)
        const stats = await fs.stat(filePath)
        if (now - stats.mtimeMs > limit) {
          await fs.unlink(filePath)
          count++
        }
      }
      return count
    } catch (error) {
      // Ignore if directory doesn't exist or other file error
      return 0
    }
  }
}
