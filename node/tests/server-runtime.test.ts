import { withServerRuntimeGuard } from "../src/runtime/server.js"

const BROWSER_LIKE_GLOBALS = ["window", "document", "navigator", "self"] as const
type BrowserLikeGlobal = typeof BROWSER_LIKE_GLOBALS[number]

describe("withServerRuntimeGuard", () => {
  it("hides browser-like globals during server-only bootstrap and restores them afterward", () => {
    const snapshots = new Map<BrowserLikeGlobal, PropertyDescriptor | undefined>(
      BROWSER_LIKE_GLOBALS.map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)]),
    )

    Object.defineProperty(globalThis, "window", {
      configurable: true,
      enumerable: true,
      writable: true,
      value: { document: {} },
    })
    Object.defineProperty(globalThis, "document", {
      configurable: true,
      enumerable: true,
      writable: true,
      value: { body: {} },
    })
    Object.defineProperty(globalThis, "self", {
      configurable: true,
      enumerable: true,
      writable: true,
      value: globalThis,
    })

    try {
      const observed = withServerRuntimeGuard(() =>
        BROWSER_LIKE_GLOBALS.map(name => typeof (globalThis as Record<BrowserLikeGlobal, unknown>)[name]),
      )

      expect(observed).toEqual(["undefined", "undefined", "undefined", "undefined"])
      expect((globalThis as typeof globalThis & { window?: { document?: unknown } }).window?.document).toEqual({})
      expect((globalThis as typeof globalThis & { document?: { body?: unknown } }).document?.body).toEqual({})
      expect((globalThis as typeof globalThis & { self?: unknown }).self).toBe(globalThis)
    } finally {
      for (const name of BROWSER_LIKE_GLOBALS) {
        const descriptor = snapshots.get(name)
        if (descriptor) Object.defineProperty(globalThis, name, descriptor)
        else Reflect.deleteProperty(globalThis, name)
      }
    }
  })
})
