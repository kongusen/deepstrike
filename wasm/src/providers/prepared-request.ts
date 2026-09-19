import { materialOptions } from "./request-plan.js"
import type { LLMProvider, PreparedProviderRequest, ProviderRunState, RenderedContext, ToolSchema } from "../types.js"

/** JSON value snapshots sever aliases to mutable provider replay and continuation state. */
export function requestSnapshot<T>(value: T): T {
  return value === undefined ? value : JSON.parse(JSON.stringify(value)) as T
}

/** A fallback can freeze only JSON state without silently discarding execution semantics. */
function assertJsonRunState(state: ProviderRunState | undefined): void {
  if (state === undefined) return
  const seen = new WeakSet<object>()
  const reject = (path: string): never => {
    throw new TypeError(`Custom provider state at ${path} is not losslessly JSON representable; implement prepareRequest() for opaque state`)
  }
  const visit = (value: unknown, path: string): void => {
    if (value === null || typeof value === "string" || typeof value === "boolean") return
    if (typeof value === "number") {
      if (!Number.isFinite(value) || Object.is(value, -0)) reject(path)
      return
    }
    if (typeof value !== "object") reject(path)
    const object = value as object
    if (seen.has(object)) reject(path)
    seen.add(object)
    const array = Array.isArray(object)
    if (!array && Object.getPrototypeOf(object) !== Object.prototype && Object.getPrototypeOf(object) !== null) reject(path)
    if (Object.getOwnPropertySymbols(object).length) reject(path)
    const descriptors = Object.getOwnPropertyDescriptors(object)
    if (array) {
      const values = object as unknown[]
      for (let index = 0; index < values.length; index++) {
        if (!Object.hasOwn(descriptors, String(index))) reject(`${path}[${index}]`)
      }
    }
    for (const [key, descriptor] of Object.entries(descriptors)) {
      if (array && key === "length") continue
      if (array && (!/^(0|[1-9]\d*)$/.test(key) || Number(key) >= (object as unknown[]).length)) reject(`${path}.${key}`)
      if (!descriptor.enumerable || !("value" in descriptor)) reject(`${path}.${key}`)
      visit(descriptor.value, `${path}.${key}`)
    }
  }
  visit(state, "state")
}

export function prepareProviderRequest(provider: LLMProvider, context: RenderedContext, tools: ToolSchema[], options?: Record<string, unknown>, state?: ProviderRunState): PreparedProviderRequest {
  if (provider.prepareRequest) return provider.prepareRequest(context, tools, options, state)
  assertJsonRunState(state)
  const input = requestSnapshot({ context, tools, options, state })
  const replay = context.turns.map(message => provider.peekProviderReplay?.(message) ?? null)
  const frozenState = input.state
  return {
    scope: "adapter_input",
    request: requestSnapshot({ context: input.context, tools: input.tools, options: materialOptions(input.options ?? {}), replay }),
    state: requestSnapshot(frozenState ?? null),
    ...(provider.countTokens ? { countTokens: () => {
      const counted = requestSnapshot(input)
      return provider.countTokens!(counted.context, counted.tools, counted.options, counted.state)
    } } : {}),
    async *stream(signal) {
      // Keep the run-state object's identity while restoring the frozen preflight input.
      if (state && frozenState && typeof state === "object" && typeof frozenState === "object") {
        for (const key of Object.keys(state)) delete state[key]
        Object.assign(state, requestSnapshot(frozenState))
      }
      yield* provider.stream(input.context, input.tools, input.options, state ?? frozenState, signal)
    },
  }
}
