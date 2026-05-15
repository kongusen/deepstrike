const BROWSER_LIKE_GLOBALS = ["window", "document", "navigator", "self"] as const
type BrowserLikeGlobal = typeof BROWSER_LIKE_GLOBALS[number]

/**
 * Some server-side tools install browser globals into Node for DOM work. SDKs
 * that probe those globals during construction can then misclassify the host as
 * a browser, so hide them only for the synchronous server-only bootstrap and
 * restore the process exactly as we found it afterward.
 */
export function withServerRuntimeGuard<T>(fn: () => T): T {
  const snapshots = BROWSER_LIKE_GLOBALS.map(name => ({
    name,
    descriptor: Object.getOwnPropertyDescriptor(globalThis, name),
  }))

  try {
    for (const { name, descriptor } of snapshots) {
      if (!descriptor) continue
      if (descriptor.configurable) {
        Reflect.deleteProperty(globalThis, name)
      } else if ("value" in descriptor && descriptor.writable) {
        Object.defineProperty(globalThis, name, { ...descriptor, value: undefined })
      }
    }
    return fn()
  } finally {
    for (const { name, descriptor } of snapshots) {
      restoreGlobal(name, descriptor)
    }
  }
}

function restoreGlobal(name: BrowserLikeGlobal, descriptor: PropertyDescriptor | undefined): void {
  if (descriptor) Object.defineProperty(globalThis, name, descriptor)
  else Reflect.deleteProperty(globalThis, name)
}
