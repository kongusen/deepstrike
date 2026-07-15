/**
 * WebArena adapter stub — interface aligned with bfcl/gaia; not runnable without Docker.
 */

/**
 * @returns {import("../../../core/types.mjs").CapAdapter}
 */
export function createWebArenaAdapter() {
  return {
    id: "webarena",
    description: "WebArena stub (requires Docker + Playwright env; see adapters/webarena/README.md)",
    loadTasks() {
      throw new Error(
        "webarena adapter is a stub. Set up the official WebArena Docker environment, then implement loadTasks. See benchmark/capability/adapters/webarena/README.md",
      )
    },
    mkTools() {
      throw new Error("webarena adapter is a stub — browser tools not wired yet")
    },
    grade() {
      return {
        passed: false,
        score: 0,
        reason: "webarena stub — no grader",
      }
    },
    maxTurns: 30,
    maxTokens: 8192,
    timeoutMs: 600_000,
  }
}
