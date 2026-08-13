import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import test from "node:test"

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..")

function run(...args) {
  return spawnSync(process.execPath, ["scripts/run-sdk-conformance.mjs", ...args], {
    cwd: root,
    encoding: "utf8",
  })
}

test("SPC-017 validates every checked-in fixture without starting an SDK", () => {
  const result = run("--validate-only")
  assert.equal(result.status, 0, result.stderr)
  const count = readdirSync(join(root, "tests", "fixtures", "sdk-conformance", "v1"))
    .filter(name => name.endsWith(".json")).length
  assert.match(result.stdout, new RegExp(`Validated ${count} SDK conformance fixtures`))
})

test("SPC-017 default selection expands to the complete SDK matrix", () => {
  const result = run("--dry-run")
  assert.equal(result.status, 0, result.stderr)
  const fixtureCount = readdirSync(join(root, "tests", "fixtures", "sdk-conformance", "v1"))
    .filter(name => name.endsWith(".json")).length
  assert.match(result.stdout, new RegExp(`Selected ${fixtureCount} SDK conformance fixtures x 4 SDKs`))
})

test("SPC-017 fixture selection fails with an actionable selector error", () => {
  const result = run("--validate-only", "--fixture", "does-not-exist")
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /Unknown fixture: does-not-exist/)
})

test("SPC-017 rejects fixture fields disallowed by the checked-in schema", () => {
  const fixtureDir = join(root, "tests", "fixtures", "sdk-conformance", "v1")
  const source = JSON.parse(readFileSync(join(fixtureDir, "agent-ir-basic.json"), "utf8"))
  const id = `schema-validation-${process.pid}`
  const path = join(fixtureDir, `${id}.json`)
  source.id = id
  source.expected.unexpected = true
  writeFileSync(path, `${JSON.stringify(source)}\n`)
  try {
    const result = run("--validate-only")
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, new RegExp(`${id}\\.json: unknown field /expected/unexpected`))
  } finally {
    rmSync(path, { force: true })
  }
})

test("SPC-017 rejects empty structured-error codes required by the schema", () => {
  const fixtureDir = join(root, "tests", "fixtures", "sdk-conformance", "v1")
  const source = JSON.parse(readFileSync(join(fixtureDir, "provider-error-unknown-stop.json"), "utf8"))
  const id = `schema-empty-error-${process.pid}`
  const path = join(fixtureDir, `${id}.json`)
  source.id = id
  source.expected.error.code = ""
  writeFileSync(path, `${JSON.stringify(source)}\n`)
  try {
    const result = run("--validate-only")
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, new RegExp(`${id}\\.json: /expected/error/code must be a non-empty string`))
  } finally {
    rmSync(path, { force: true })
  }
})

test("SPC-017 rejects malformed JSON Pointers in structured errors", () => {
  const fixtureDir = join(root, "tests", "fixtures", "sdk-conformance", "v1")
  const source = JSON.parse(readFileSync(join(fixtureDir, "provider-error-unknown-stop.json"), "utf8"))
  const id = `schema-invalid-pointer-${process.pid}`
  const path = join(fixtureDir, `${id}.json`)
  source.id = id
  source.expected.error.path = "/bad~pointer"
  writeFileSync(path, `${JSON.stringify(source)}\n`)
  try {
    const result = run("--validate-only")
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, new RegExp(`${id}\\.json: /expected/error/path must be a JSON Pointer`))
  } finally {
    rmSync(path, { force: true })
  }
})

test("SPC-017 enforces every constrained fixture schema field", () => {
  const fixtureDir = join(root, "tests", "fixtures", "sdk-conformance", "v1")
  const source = JSON.parse(readFileSync(join(fixtureDir, "agent-ir-basic.json"), "utf8"))
  const cases = [
    {
      name: "input type",
      mutate: value => { value.input = [] },
      expected: "/input must be an object",
    },
    {
      name: "canonical type",
      mutate: value => { value.expected.canonical = [] },
      expected: "/expected/canonical must be an object",
    },
    {
      name: "positive integer contract version",
      mutate: value => { value.expected.contractVersion = 0 },
      expected: "/expected.contractVersion must be a positive integer",
    },
    {
      name: "one expected result variant",
      mutate: value => { value.expected.error = { code: "unexpected", path: "" } },
      expected: "/expected must contain exactly one of canonical or error",
    },
    {
      name: "error type",
      mutate: value => {
        delete value.expected.canonical
        value.expected.error = []
      },
      expected: "/expected/error must be an object",
    },
    {
      name: "schema version const",
      mutate: value => { value.schemaVersion = 2 },
      expected: "/schemaVersion must equal 1",
    },
    {
      name: "domain enum",
      mutate: value => { value.domain = "unknown" },
      expected: "unsupported /domain unknown",
    },
  ]
  for (const { name, mutate, expected } of cases) {
    const id = `schema-${name.replaceAll(" ", "-")}-${process.pid}`
    const path = join(fixtureDir, `${id}.json`)
    const value = structuredClone(source)
    value.id = id
    mutate(value)
    writeFileSync(path, `${JSON.stringify(value)}\n`)
    try {
      const result = run("--validate-only")
      assert.notEqual(result.status, 0, name)
      assert.ok(result.stderr.includes(`${id}.json: ${expected}`), `${name}: ${result.stderr}`)
    } finally {
      rmSync(path, { force: true })
    }
  }
})
