import { execFileSync } from "node:child_process"
import { mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { fileURLToPath } from "node:url"
import { dirname, join, resolve } from "node:path"

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..")
const fixturesDir = join(root, "tests", "fixtures", "sdk-conformance", "canonical")
const adapter = join(root, "scripts", "sdk-conformance", "node-adapter.mjs")

function runFixture(path: string): Record<string, unknown> {
  const stdout = execFileSync(process.execPath, [adapter, path], { encoding: "utf8" })
  expect(stdout.trim().split(/\r?\n/)).toHaveLength(1)
  return JSON.parse(stdout) as Record<string, unknown>
}

function runAdapterFixture(fixture: Record<string, unknown>): Record<string, unknown> {
  const directory = mkdtempSync(join(tmpdir(), "deepstrike-sdk-conformance-"))
  const path = join(directory, "fixture.json")
  writeFileSync(path, `${JSON.stringify(fixture)}\n`)
  try {
    return runFixture(path)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

function promptMeasurementFixture(expectedCanonical: Record<string, unknown>): Record<string, unknown> {
  return {
    id: "adapter-focused",
    domain: "prompt_measurement",
    input: {
      value: {
        requestFingerprint: "sha256:focused",
        inputTokens: 12,
        source: { kind: "heuristic" },
        confidence: "low_confidence",
      },
    },
    expected: { canonical: expectedCanonical },
  }
}

function agentIrFixture(reference: string): Record<string, unknown> {
  return {
    id: "adapter-focused",
    domain: "agent_ir",
    input: { fixture: reference },
    expected: { canonical: {} },
  }
}

describe("spc_017 Node SDK conformance adapter", () => {
  for (const name of readdirSync(fixturesDir).filter(name => name.endsWith(".json")).sort()) {
    it(`matches ${name}`, async () => {
      const path = join(fixturesDir, name)
      const fixture = JSON.parse(await (await import("node:fs/promises")).readFile(path, "utf8")) as {
        id: string
        expected: { canonical?: Record<string, unknown>; error?: { code: string; path: string } }
      }
      const envelope = runFixture(path)

      expect(envelope.sdk).toBe("node")
      expect(envelope.fixture).toBe(fixture.id)
      if (fixture.expected.canonical) {
        expect(envelope).toEqual({
          ok: true,
          sdk: "node",
          fixture: fixture.id,
          canonical: fixture.expected.canonical,
        })
      } else {
        expect(envelope).toMatchObject({
          ok: false,
          sdk: "node",
          fixture: fixture.id,
          error: fixture.expected.error,
        })
        expect((envelope.error as { message?: unknown }).message).toEqual(expect.any(String))
      }
    })
  }

  it("derives canonical output from the SDK instead of expected fixture shape", () => {
    const envelope = runAdapterFixture(promptMeasurementFixture({
      requestFingerprint: "sha256:focused",
      inputTokens: 12,
      source: { kind: "heuristic" },
      confidence: "low_confidence",
      unexpectedList: ["must-not-be-copied"],
    }))

    expect(envelope).toEqual({
      ok: true,
      sdk: "node",
      fixture: "adapter-focused",
      canonical: {
        requestFingerprint: "sha256:focused",
        inputTokens: 12,
        source: { kind: "heuristic" },
        confidence: "low_confidence",
      },
    })
  })

  it.each([
    ["absolute path", resolve(root, "tests", "fixtures", "agent-ir", "canonical-agent.json")],
    ["UNC path", "\\\\server\\share\\agent.json"],
    ["fixtures root", "."],
    ["parent traversal", "agent-ir/../agent-ir/canonical-agent.json"],
  ])("rejects an input.fixture %s", (_label, reference) => {
    const envelope = runAdapterFixture(agentIrFixture(reference))

    expect(envelope).toMatchObject({
      ok: false,
      sdk: "node",
      fixture: "adapter-focused",
      error: { code: "invalid_fixture_reference", path: "/input/fixture" },
    })
  })

  it("rejects an input.fixture symlink that resolves outside tests/fixtures", () => {
    const directory = mkdtempSync(join(tmpdir(), "deepstrike-sdk-conformance-source-"))
    const outside = join(directory, "agent.json")
    const link = join(root, "tests", "fixtures", `.sdk-conformance-escape-${process.pid}-${Date.now()}.json`)
    writeFileSync(outside, readFileSync(join(root, "tests", "fixtures", "agent-ir", "canonical-agent.json")))
    symlinkSync(outside, link)
    try {
      const envelope = runAdapterFixture(agentIrFixture(link.slice(join(root, "tests", "fixtures").length + 1)))
      expect(envelope).toMatchObject({
        ok: false,
        sdk: "node",
        fixture: "adapter-focused",
        error: { code: "invalid_fixture_reference", path: "/input/fixture" },
      })
    } finally {
      unlinkSync(link)
      rmSync(directory, { recursive: true, force: true })
    }
  })
})
