/**
 * Map BFCL function schemas → deepstrike tool() registrations (mock execute).
 */

/**
 * @param {import("../../../core/types.mjs").CapTask} task
 * @param {any} sdk
 */
export function mkBfclTools(task, sdk) {
  const { tool } = sdk
  const funcs = task.functions ?? []
  return funcs.map(fn => {
    const name = String(fn.name)
    const description = String(fn.description ?? `Function ${name}`)
    const parameters = normalizeParameters(fn.parameters)
    return tool(name, description, parameters, async args => {
      return JSON.stringify({ ok: true, tool: name, args })
    })
  })
}

/** @param {unknown} parameters */
function normalizeParameters(parameters) {
  if (!parameters || typeof parameters !== "object") {
    return { type: "object", properties: {} }
  }
  const p = /** @type {Record<string, unknown>} */ (parameters)
  // Official BFCL sometimes nests under { type: "dict", properties: { properties: … } }
  // Smoke tasks use standard JSON Schema object form.
  if (p.type === "dict" && p.properties && typeof p.properties === "object") {
    const nested = /** @type {Record<string, unknown>} */ (p.properties)
    if (nested.properties) {
      return {
        type: "object",
        properties: nested.properties,
        ...(Array.isArray(nested.required) ? { required: nested.required } : {}),
      }
    }
  }
  return {
    type: p.type === "dict" ? "object" : (p.type ?? "object"),
    properties: p.properties ?? {},
    ...(Array.isArray(p.required) ? { required: p.required } : {}),
  }
}
