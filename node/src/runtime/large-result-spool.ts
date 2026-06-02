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

  constructor(config: Partial<SpoolConfig> = {}) {
    this.config = { ...DEFAULT_SPOOL_CONFIG, ...config }
    this.spoolDir = '.spool'
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

  /**
   * Write large result to disk.
   */
  private async writeToDisk(
    content: string,
    hash: string
  ): Promise<string> {
    const spoolPath = this.getSpoolPath(hash)

    // Ensure spool directory exists
    await fs.mkdir(this.spoolDir, { recursive: true })

    // Write full content to disk
    await fs.writeFile(spoolPath, content, 'utf-8')

    return spoolPath
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
  async persistOutput(callId: string, content: string): Promise<string> {
    const hash = this.hashContent(content)
    const spoolPath = this.getSpoolPath(`${callId}-${hash.slice(0, 16)}`)
    await fs.mkdir(this.spoolDir, { recursive: true })
    await fs.writeFile(spoolPath, content, "utf-8")
    return spoolPath
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
   * Clean up old spool files (optional maintenance).
   */
  async cleanup(maxAgeMs: number = 7 * 24 * 60 * 60 * 1000): Promise<number> {
    // Not implemented for now - future enhancement
    return 0
  }
}
