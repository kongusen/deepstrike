import { readdir, stat } from "fs/promises"
import { join } from "path"

export type InboxCallback = (paths: string[]) => Promise<void>

/** Poll inbox/ every intervalMs, call callback with new file paths. */
export function startInboxWatcher(inboxDir: string, callback: InboxCallback, intervalMs = 30_000): NodeJS.Timeout {
  const seen = new Set<string>()

  const scan = async () => {
    try {
      const entries = await readdir(inboxDir)
      const newPaths: string[] = []
      for (const entry of entries) {
        if (entry.startsWith(".")) continue
        if (seen.has(entry)) continue
        const full = join(inboxDir, entry)
        try {
          const s = await stat(full)
          if (s.isFile()) {
            seen.add(entry)
            newPaths.push(full)
          }
        } catch { /* file may have been removed */ }
      }
      if (newPaths.length) await callback(newPaths)
    } catch { /* inbox dir may not exist yet */ }
  }

  // Immediate first scan
  void scan()
  return setInterval(scan, intervalMs)
}
