// G2 deterministic compute: the host-side reducer registry. A `NodeKind::Reduce` workflow node runs
// no LLM agent — the kernel hands the SDK a reducer name + its dependency outputs, and the SDK runs
// the named pure function here. This is the "ordinary code between stages" (dedupe / filter / merge /
// early-exit) of the code-orchestration model, expressed deterministically as a DAG node.
import { extractJsonValue } from "./output-schema.js"

/** One dependency's contribution to a reduce: the producing node's agent id and its output text. */
export interface ReducerInput {
  agentId: string
  output: string
}

/** A pure function over a reduce node's dependency outputs → the reduce node's output string. */
export type Reducer = (inputs: ReducerInput[]) => string

export type ReducerRegistry = Record<string, Reducer>

/** Non-empty, trimmed lines of a string. */
function lines(s: string): string[] {
  return s
    .split("\n")
    .map(l => l.trim())
    .filter(l => l.length > 0)
}

/**
 * Built-in reducers, available to every workflow without registration. A user-supplied registry is
 * merged over these (so a custom reducer can shadow a built-in of the same name).
 */
export const builtinReducers: ReducerRegistry = {
  /** Concatenate every input's output, separated by blank lines, in dependency order. */
  concat: inputs => inputs.map(i => i.output).join("\n\n"),

  /** Union of non-empty lines across all inputs, first-seen order preserved (dedupe a fan-out). */
  dedupe_lines: inputs => {
    const seen = new Set<string>()
    const out: string[] = []
    for (const i of inputs) {
      for (const line of lines(i.output)) {
        if (!seen.has(line)) {
          seen.add(line)
          out.push(line)
        }
      }
    }
    return out.join("\n")
  },

  /** Parse each input as a JSON array, concatenate, dedupe by canonical JSON → a JSON array string. */
  merge_json_arrays: inputs => {
    const seen = new Set<string>()
    const merged: unknown[] = []
    for (const i of inputs) {
      const v = extractJsonValue(i.output)
      const arr = Array.isArray(v) ? v : v !== undefined ? [v] : []
      for (const el of arr) {
        const key = JSON.stringify(el)
        if (!seen.has(key)) {
          seen.add(key)
          merged.push(el)
        }
      }
    }
    return JSON.stringify(merged)
  },

  /** The number of inputs that produced any non-empty output — handy for early-exit/branch gates. */
  count: inputs => String(inputs.filter(i => i.output.trim().length > 0).length),
}

/** Resolve a reducer by name from the built-ins overlaid with a user registry. */
export function resolveReducer(name: string, user?: ReducerRegistry): Reducer | undefined {
  return user?.[name] ?? builtinReducers[name]
}
