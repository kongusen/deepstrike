import { readFile } from "node:fs/promises"
import { resolve } from "node:path"
import { pathToFileURL } from "node:url"

export const EXPECTED_SDK_VERSION = "0.2.74"

const SURFACE_FILES = {
  root: "index.js",
  providers: "providers/public.js",
  workflow: "workflow/public.js",
  planes: "planes/public.js",
  memory: "memory/public.js",
  harness: "harness/public.js",
  os: "os/public.js",
  advanced: "advanced/public.js",
  runtime: "runtime/public.js",
  evals: "evals/public.js",
}

export async function loadSdk(options = {}) {
  const repoRoot = options.repoRoot ?? resolve(new URL("../..", import.meta.url).pathname)
  const nodeRoot = resolve(repoRoot, "node")
  const packageJson = JSON.parse(await readFile(resolve(nodeRoot, "package.json"), "utf8"))
  const versionFile = (await readFile(resolve(repoRoot, "VERSION"), "utf8")).trim()
  const distRoot = resolve(nodeRoot, "dist")
  const surfaces = {}
  for (const [name, file] of Object.entries(SURFACE_FILES)) {
    surfaces[name] = await import(pathToFileURL(resolve(distRoot, file)).href)
  }
  return {
    version: packageJson.version,
    versionFile,
    packageJson,
    nodeRoot,
    distRoot,
    surfaces,
    root: surfaces.root,
    ...surfaces,
  }
}

export function assertSdkVersion(sdk, expected = EXPECTED_SDK_VERSION) {
  if (sdk.version !== expected || sdk.versionFile !== expected) {
    throw new Error(`SDK version mismatch: package=${sdk.version}, VERSION=${sdk.versionFile}, expected=${expected}`)
  }
  return sdk
}

export function surfaceNames(sdk) {
  return Object.keys(sdk.surfaces ?? {})
}
