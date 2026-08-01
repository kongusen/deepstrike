import * as crypto from "node:crypto"
import * as fs from "node:fs/promises"
import * as path from "node:path"

export interface PayloadStoreConfig {
  storageDir?: string
  maxAgeMs?: number
}

/** File-backed storage for canonical opaque payload locators. */
export class PayloadStore {
  private readonly storageDir: string
  private readonly maxAgeMs?: number
  private readonly activeWrites = new Map<string, Promise<void>>()

  constructor(config: PayloadStoreConfig = {}) {
    this.storageDir = config.storageDir ?? ".payloads"
    this.maxAgeMs = config.maxAgeMs
  }

  private payloadPath(sessionId: string, payloadRef: string): string {
    const key = crypto
      .createHash("sha256")
      .update(`${sessionId}\u0000${payloadRef}`)
      .digest("hex")
    return path.join(this.storageDir, `${key}.payload`)
  }

  async persistPayload(sessionId: string, payloadRef: string, content: string): Promise<void> {
    const target = this.payloadPath(sessionId, payloadRef)
    let write = this.activeWrites.get(target)
    if (!write) {
      write = this.atomicWrite(target, content).finally(() => {
        this.activeWrites.delete(target)
      })
      this.activeWrites.set(target, write)
    }
    await write
  }

  async loadPayload(sessionId: string, payloadRef: string): Promise<string | undefined> {
    try {
      return await fs.readFile(this.payloadPath(sessionId, payloadRef), "utf8")
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined
      throw error
    }
  }

  async cleanup(maxAgeMs = this.maxAgeMs ?? 7 * 24 * 60 * 60 * 1000): Promise<number> {
    let files: string[]
    try {
      files = await fs.readdir(this.storageDir)
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return 0
      throw error
    }

    const now = Date.now()
    let removed = 0
    for (const file of files) {
      const target = path.join(this.storageDir, file)
      const stat = await fs.stat(target)
      if (now - stat.mtimeMs > maxAgeMs) {
        await fs.unlink(target)
        removed += 1
      }
    }
    return removed
  }

  private async atomicWrite(target: string, content: string): Promise<void> {
    await fs.mkdir(this.storageDir, { recursive: true })
    const temporary = `${target}.${process.pid}.${crypto.randomUUID()}.tmp`
    let handle: fs.FileHandle | undefined
    try {
      handle = await fs.open(temporary, "wx")
      await handle.writeFile(content, "utf8")
      await handle.sync()
      await handle.close()
      handle = undefined
      await fs.rename(temporary, target)
    } finally {
      await handle?.close().catch(() => undefined)
      await fs.unlink(temporary).catch(() => undefined)
    }
  }
}
