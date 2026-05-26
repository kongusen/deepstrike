import { watch, type FSWatcher } from "fs"
import { resolve, extname } from "path"

const WATCHED_EXTS = new Set([".md", ".json", ".py"])

export type SkillChangeHandler = (skillDir: string) => void | Promise<void>

/**
 * Watch a skill directory for changes and invoke `onChanged` whenever a skill
 * file (`.md`, `.json`, or `.py`) is created, modified, or removed.
 *
 * Uses Node.js native `fs.watch` — no extra dependencies.
 * The returned handle should be closed when the runner terminates.
 *
 * ```ts
 * const watcher = watchSkillDir(opts.skillDir, async (dir) => {
 *   const metas = await scanSkillDir(dir)
 *   runtime.updateAvailableSkills(metas)
 * })
 * // later:
 * watcher.close()
 * ```
 */
export function watchSkillDir(skillDir: string, onChanged: SkillChangeHandler): FSWatcher {
  const dir = resolve(skillDir)
  let debounceTimer: ReturnType<typeof setTimeout> | null = null

  const watcher = watch(dir, { persistent: false }, (_eventType, filename) => {
    if (!filename) return
    if (!WATCHED_EXTS.has(extname(filename))) return

    // Debounce: collapse rapid bursts (e.g. editor save-then-format) into one
    // reload tick with a 200 ms window.
    if (debounceTimer !== null) clearTimeout(debounceTimer)
    debounceTimer = setTimeout(() => {
      debounceTimer = null
      Promise.resolve(onChanged(dir)).catch(() => {/* caller handles errors */})
    }, 200)
  })

  return watcher
}
