import { mkdir, writeFile, readFile } from "node:fs/promises"
import { join } from "node:path"
import type { ModelMessage } from "../types.js"

export interface ArchiveStore {
  write(sessionId: string, seq: number, messages: ModelMessage[]): Promise<string>
  read(archiveRef: string): Promise<ModelMessage[]>
}

export class NullArchiveStore implements ArchiveStore {
  async write(_sessionId: string, _seq: number, _messages: ModelMessage[]): Promise<string> {
    return ""
  }

  async read(_archiveRef: string): Promise<ModelMessage[]> {
    throw new Error("NullArchiveStore does not store archives")
  }
}

export class FileArchiveStore implements ArchiveStore {
  constructor(private readonly root: string) {}

  async write(sessionId: string, seq: number, messages: ModelMessage[]): Promise<string> {
    const dir = join(this.root, sessionId)
    await mkdir(dir, { recursive: true })
    const filePath = join(dir, `${seq}.jsonl`)
    const lines = messages.map(msg => JSON.stringify(msg)).join("\n") + "\n"
    await writeFile(filePath, lines, "utf8")
    return filePath
  }

  async read(archiveRef: string): Promise<ModelMessage[]> {
    const content = await readFile(archiveRef, "utf8")
    const lines = content.split("\n")
    const messages: ModelMessage[] = []
    for (const line of lines) {
      if (!line.trim()) continue
      messages.push(JSON.parse(line) as ModelMessage)
    }
    return messages
  }
}
