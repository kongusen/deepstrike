/**
 * GAIA smoke tools: virtual FS read_file + code_exec + search stub.
 */

/**
 * @param {import("../../../core/types.mjs").CapTask} task
 * @param {any} sdk
 */
export function mkGaiaTools(task, sdk) {
  const { tool } = sdk
  const files = /** @type {Record<string, string>} */ (task.meta?.files ?? {})
  const searchHits = /** @type {Record<string, Array<{ title: string, snippet: string }>>} */ (
    task.meta?.searchHits ?? {}
  )

  return [
    tool(
      "read_file",
      "Read a text file from the task workspace by path.",
      {
        type: "object",
        properties: { path: { type: "string", description: "Relative file path" } },
        required: ["path"],
      },
      async args => {
        const p = String(args.path ?? "").replace(/^\.\//, "")
        if (p in files) return files[p]
        // Allow basename match
        const hit = Object.entries(files).find(([k]) => k === p || k.endsWith("/" + p) || k.endsWith(p))
        if (hit) return hit[1]
        return JSON.stringify({ error: `file not found: ${p}`, available: Object.keys(files) })
      },
    ),
    tool(
      "code_exec",
      "Evaluate a short JavaScript expression and return its result as a string. Use for arithmetic.",
      {
        type: "object",
        properties: {
          code: { type: "string", description: "JavaScript expression or statements; last expression is returned" },
        },
        required: ["code"],
      },
      async args => {
        const code = String(args.code ?? "")
        try {
          // Restricted: Function constructor, no require/process. Smoke-only.
          // eslint-disable-next-line no-new-func
          const fn = new Function(`"use strict"; return (${code})`)
          const result = fn()
          return String(result)
        } catch (e1) {
          try {
            // eslint-disable-next-line no-new-func
            const fn2 = new Function(`"use strict"; ${code}`)
            const result = fn2()
            return result === undefined ? "undefined" : String(result)
          } catch (e2) {
            return JSON.stringify({ error: e2?.message ? String(e2.message) : String(e2) })
          }
        }
      },
    ),
    tool(
      "search",
      "Search a small local knowledge stub (not the live web). Returns titles and snippets.",
      {
        type: "object",
        properties: {
          query: { type: "string", description: "Search query" },
        },
        required: ["query"],
      },
      async args => {
        const q = String(args.query ?? "")
        // Exact key match first, then substring
        if (searchHits[q]) return JSON.stringify({ query: q, results: searchHits[q] })
        const key = Object.keys(searchHits).find(k =>
          k.toLowerCase().includes(q.toLowerCase()) || q.toLowerCase().includes(k.toLowerCase()),
        )
        if (key) return JSON.stringify({ query: q, results: searchHits[key] })
        return JSON.stringify({ query: q, results: [], note: "no stub hits" })
      },
    ),
  ]
}
