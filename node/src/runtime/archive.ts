import { mkdir, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Message } from "../types.js"

export interface ArchiveStore {
  write(sessionId: string, seq: number, messages: Message[]): Promise<string>
}

export class NullArchiveStore implements ArchiveStore {
  async write(_sessionId: string, _seq: number, _messages: Message[]): Promise<string> {
    return ""
  }
}

export class FileArchiveStore implements ArchiveStore {
  constructor(private readonly root: string) {}

  async write(sessionId: string, seq: number, messages: Message[]): Promise<string> {
    const dir = join(this.root, sessionId)
    await mkdir(dir, { recursive: true })
    const filePath = join(dir, `${seq}.jsonl`)
    const lines = messages.map(msg => JSON.stringify(msg)).join("\n") + "\n"
    await writeFile(filePath, lines, "utf8")
    return filePath
  }
}
