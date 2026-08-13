#!/usr/bin/env node
/**
 * SPC-017 differential runner. SDK adapters are intentionally small CLI programs: this runner
 * owns fixture validation, build preparation, and comparison so an SDK cannot declare itself
 * conformant by itself or run against stale generated output.
 */
import { existsSync, readdirSync, readFileSync } from "node:fs"
import { spawnSync } from "node:child_process"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const fixturesRoot = join(root, "tests", "fixtures", "sdk-conformance")
const fixtureDir = join(fixturesRoot, "v1")
const schema = json(join(fixturesRoot, "schema.json"))
const args = parseArgs(process.argv.slice(2))
const python = process.env.DEEPSTRIKE_CONFORMANCE_PYTHON
  ?? join(root, "python", ".venv", "bin", "python")
const maturin = process.env.DEEPSTRIKE_CONFORMANCE_MATURIN
  ?? join(dirname(python), "maturin")
const wasmPackageDir = join(root, "crates", "deepstrike-wasm", "pkg")
const wasmSdkVersion = json(join(root, "wasm", "package.json")).version

const adapters = {
  node: [process.execPath, "scripts/sdk-conformance/node-adapter.mjs"],
  python: [python, "scripts/sdk-conformance/python-adapter.py"],
  wasm: [process.execPath, "scripts/sdk-conformance/wasm.mjs"],
  rust: ["cargo", "run", "--quiet", "-p", "deepstrike-sdk", "--bin", "sdk-conformance", "--"],
}

const fixtures = loadFixtures()
const selected = selectFixtures(fixtures, args.fixture)
const selectedSdks = args.sdk.length ? args.sdk.map(name => {
  if (!adapters[name]) fail(`Unknown SDK: ${name}`)
  return name
}) : Object.keys(adapters)
if (args.validateOnly) {
  console.log(`Validated ${selected.length} SDK conformance fixtures`)
  process.exit(0)
}
if (args.dryRun) {
  console.log(`Selected ${selected.length} SDK conformance fixtures x ${selectedSdks.length} SDKs`)
  process.exit(0)
}

buildSelectedSdks(selectedSdks)

let failures = 0
for (const fixture of selected) {
  for (const sdk of selectedSdks) {
    const actual = runAdapter(sdk, fixture.path)
    const difference = compareEnvelope(fixture.value, actual, sdk)
    if (difference) {
      failures += 1
      console.error(`FAIL ${sdk} ${fixture.value.id} ${difference.path}: ${difference.message}`)
      continue
    }
    console.log(`OK   ${sdk} ${fixture.value.id}`)
  }
}

if (failures) {
  console.error(`\n${failures} conformance difference(s)`)
  process.exit(1)
}
console.log(`\nCross-SDK conformance passed (${selected.length} fixtures x ${selectedSdks.length} SDKs)`)

function loadFixtures() {
  const fixtures = readdirSync(fixtureDir)
    .filter(name => name.endsWith(".json"))
    .sort()
    .map(name => ({ path: join(fixtureDir, name), value: json(join(fixtureDir, name)) }))
  if (!fixtures.length) fail("No SDK conformance fixtures found")
  for (const fixture of fixtures) validateFixture(fixture)
  return fixtures
}

function selectFixtures(fixtures, selectors) {
  if (!selectors?.length) return fixtures
  const selected = []
  for (const selector of selectors) {
    const fixture = fixtures.find(item => item.value.id === selector)
    if (!fixture) fail(`Unknown fixture: ${selector}`)
    selected.push(fixture)
  }
  return selected
}

function validateFixture({ path, value }) {
  const label = path.slice(root.length + 1)
  if (!isObject(value)) fail(`${label}: / must be an object`)
  requireOnlyKeys(value, schema.properties, label, "/")
  requireKeys(value, schema.required, label, "/")
  if (value.schemaVersion !== schema.properties.schemaVersion.const) {
    fail(`${label}: /schemaVersion must equal ${schema.properties.schemaVersion.const}`)
  }
  if (typeof value.id !== "string" || !new RegExp(schema.properties.id.pattern).test(value.id)) {
    fail(`${label}: invalid /id`)
  }
  if (resolve(fixtureDir, `${value.id}.json`) !== path) fail(`${label}: /id must match filename`)
  if (!schema.properties.domain.enum.includes(value.domain)) fail(`${label}: unsupported /domain ${String(value.domain)}`)
  if (!isObject(value.input)) fail(`${label}: /input must be an object`)
  validateExpected(value.expected, label)
}

function validateExpected(expected, label) {
  const expectedSchema = schema.properties.expected
  if (!isObject(expected)) fail(`${label}: /expected must be an object`)
  requireOnlyKeys(expected, expectedSchema.properties, label, "/expected")
  requireKeys(expected, expectedSchema.required, label, "/expected")
  if (!Number.isInteger(expected.contractVersion) || expected.contractVersion < 1) {
    fail(`${label}: /expected.contractVersion must be a positive integer`)
  }
  const hasCanonical = Object.hasOwn(expected, "canonical")
  const hasError = Object.hasOwn(expected, "error")
  if (hasCanonical === hasError) fail(`${label}: /expected must contain exactly one of canonical or error`)
  if (hasCanonical && !isObject(expected.canonical)) fail(`${label}: /expected/canonical must be an object`)
  if (hasError) {
    const errorSchema = expectedSchema.properties.error
    if (!isObject(expected.error)) fail(`${label}: /expected/error must be an object`)
    requireOnlyKeys(expected.error, errorSchema.properties, label, "/expected/error")
    requireKeys(expected.error, errorSchema.required, label, "/expected/error")
    if (typeof expected.error.code !== "string" || !expected.error.code.length) {
      fail(`${label}: /expected/error/code must be a non-empty string`)
    }
    if (!isJsonPointer(expected.error.path)) fail(`${label}: /expected/error/path must be a JSON Pointer`)
  }
}

function requireKeys(value, keys, label, path) {
  for (const key of keys) {
    if (!Object.hasOwn(value, key)) fail(`${label}: missing required field ${pointer(path, key)}`)
  }
}

function requireOnlyKeys(value, properties, label, path) {
  for (const key of Object.keys(value)) {
    if (!Object.hasOwn(properties, key)) fail(`${label}: unknown field ${pointer(path, key)}`)
  }
}

function pointer(base, key) {
  return `${base === "/" ? "" : base}/${escapePointer(key)}`
}

function buildSelectedSdks(sdks) {
  if (sdks.includes("node")) runBuild("Node SDK", "npm", ["--prefix", "node", "run", "build"])
  if (sdks.includes("wasm")) buildWasmSdk()
  if (sdks.includes("python")) {
    if (!existsSync(python)) fail(`Python conformance environment is missing: ${python}`)
    if (!existsSync(maturin)) fail(`Maturin is missing from the Python conformance environment: ${maturin}`)
    const venv = dirname(dirname(python))
    runBuild("Python SDK", maturin, ["develop", "--release"], {
      cwd: join(root, "python"),
      env: { ...process.env, VIRTUAL_ENV: venv, PATH: `${dirname(python)}:${process.env.PATH ?? ""}` },
    })
  }
}

function buildWasmSdk() {
  runBuild("WASM kernel", "wasm-pack", [
    "build", "--release", "--target", "bundler", "--out-dir", "pkg", "--scope", "deepstrike",
  ], { cwd: join(root, "crates", "deepstrike-wasm") })
  runBuild("WASM kernel metadata", process.execPath, [
    "scripts/normalize-wasm-kernel-package.mjs", wasmSdkVersion, wasmPackageDir,
  ])
  runBuild("WASM kernel link", "npm", [
    "--prefix", "wasm", "install", "--no-save", "--package-lock=false", wasmPackageDir,
  ])
  runBuild("WASM SDK", "npm", ["--prefix", "wasm", "run", "build"])
}

function runBuild(label, command, commandArgs, options = {}) {
  const result = spawnSync(command, commandArgs, {
    cwd: root,
    encoding: "utf8",
    ...options,
  })
  if (result.error) fail(`${label} build could not start: ${result.error.message}`)
  if (result.status !== 0) {
    const output = [result.stdout, result.stderr].filter(Boolean).join("\n").trim()
    fail(`${label} build failed${output ? `:\n${output}` : ""}`)
  }
}

function runAdapter(sdk, fixture) {
  const [command, ...commandArgs] = adapters[sdk]
  const result = spawnSync(command, [...commandArgs, fixture], {
    cwd: root,
    encoding: "utf8",
  })
  if (result.error) return adapterFailure(sdk, `could not start adapter: ${result.error.message}`)
  if (result.status !== 0) return adapterFailure(sdk, `adapter exited ${result.status}: ${result.stderr.trim()}`)
  const lines = result.stdout.trim().split("\n").filter(Boolean)
  if (lines.length !== 1) return adapterFailure(sdk, `adapter must emit exactly one JSON line, got ${lines.length}`)
  try {
    return JSON.parse(lines[0])
  } catch (error) {
    return adapterFailure(sdk, `invalid adapter JSON: ${error.message}`)
  }
}

function compareEnvelope(fixture, actual, sdk) {
  if (!isObject(actual)) return diff("", "adapter output is not an object")
  if (actual.sdk !== sdk) return diff("/sdk", `expected ${sdk}, got ${String(actual.sdk)}`)
  if (actual.fixture !== fixture.id) return diff("/fixture", `expected ${fixture.id}, got ${String(actual.fixture)}`)
  if (actual.contractVersion !== fixture.expected.contractVersion) {
    return diff("/contractVersion", `expected ${fixture.expected.contractVersion}, got ${String(actual.contractVersion)}`)
  }
  if (fixture.expected.error) {
    if (actual.ok !== false || !isObject(actual.error)) return diff("/ok", "expected a structured error envelope")
    if (actual.error.code !== fixture.expected.error.code) return diff("/error/code", `expected ${fixture.expected.error.code}, got ${String(actual.error.code)}`)
    if (actual.error.path !== fixture.expected.error.path) return diff("/error/path", `expected ${fixture.expected.error.path}, got ${String(actual.error.path)}`)
    return undefined
  }
  if (actual.ok !== true) return diff("/ok", `expected success, got ${String(actual.ok)}`)
  return firstDifference(fixture.expected.canonical, actual.canonical)
}

function firstDifference(expected, actual, path = "") {
  if (Object.is(expected, actual)) return undefined
  if (Array.isArray(expected) && Array.isArray(actual)) {
    if (expected.length !== actual.length) return diff(path, `array length expected ${expected.length}, got ${actual.length}`)
    for (let index = 0; index < expected.length; index += 1) {
      const result = firstDifference(expected[index], actual[index], `${path}/${index}`)
      if (result) return result
    }
    return undefined
  }
  if (isObject(expected) && isObject(actual)) {
    const expectedKeys = Object.keys(expected).sort()
    const actualKeys = Object.keys(actual).sort()
    if (stableJson(expectedKeys) !== stableJson(actualKeys)) {
      return diff(path, `object keys expected ${stableJson(expectedKeys)}, got ${stableJson(actualKeys)}`)
    }
    for (const key of expectedKeys) {
      const result = firstDifference(expected[key], actual[key], `${path}/${escapePointer(key)}`)
      if (result) return result
    }
    return undefined
  }
  return diff(path, `expected ${stableJson(expected)}, got ${stableJson(actual)}`)
}

function adapterFailure(sdk, message) {
  return { ok: false, sdk, fixture: "", contractVersion: 0, error: { code: "adapter_failure", path: "", message } }
}

function parseArgs(argv) {
  const result = { sdk: [], fixture: [], validateOnly: false, dryRun: false }
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index]
    if (arg === "--validate-only") result.validateOnly = true
    else if (arg === "--dry-run") result.dryRun = true
    else if (arg === "--sdk" || arg === "--fixture") {
      const value = argv[++index]
      if (!value) fail(`${arg} requires a value`)
      result[arg.slice(2)].push(value)
    } else if (arg === "--help") {
      console.log("Usage: node scripts/run-sdk-conformance.mjs [--sdk node|python|wasm|rust] [--fixture fixture-id] [--validate-only|--dry-run]")
      process.exit(0)
    } else fail(`Unknown argument: ${arg}`)
  }
  return result
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`
  if (isObject(value)) return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`
  return JSON.stringify(Object.is(value, -0) ? 0 : value)
}

function json(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"))
  } catch (error) {
    fail(`Cannot parse ${path}: ${error.message}`)
  }
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

function isJsonPointer(value) {
  return typeof value === "string" && (value === "" || /^(?:\/(?:[^~/]|~[01])*)+$/.test(value))
}

function escapePointer(value) {
  return value.replaceAll("~", "~0").replaceAll("/", "~1")
}

function diff(path, message) {
  return { path: path || "/", message }
}

function fail(message) {
  console.error(message)
  process.exit(1)
}
