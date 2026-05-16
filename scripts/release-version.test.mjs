import assert from "node:assert/strict"
import test from "node:test"
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { syncReleaseVersion } from "./release-version.mjs"

test("syncReleaseVersion propagates the canonical version across release manifests", () => {
  const repoRoot = mkdtempSync(join(tmpdir(), "deepstrike-version-sync-"))
  writeFixture(repoRoot, "VERSION", "1.2.3\n")
  writeFixture(repoRoot, "Cargo.toml", `
[workspace.package]
version = "0.0.1"

[workspace.dependencies]
deepstrike-core = { path = "crates/deepstrike-core", version = "0.0.1" }
deepstrike-tokenizer = { path = "crates/deepstrike-tokenizer", version = "0.0.1" }
`)
  writeFixture(repoRoot, "Cargo.lock", `
[[package]]
name = "deepstrike-core"
version = "0.0.1"

[[package]]
name = "deepstrike-node"
version = "0.0.1"

[[package]]
name = "deepstrike-py"
version = "0.0.1"

[[package]]
name = "deepstrike-sdk"
version = "0.0.1"

[[package]]
name = "deepstrike-tokenizer"
version = "0.0.1"

[[package]]
name = "deepstrike-wasm"
version = "0.0.1"
`)
  writeFixture(repoRoot, "README.md", 'deepstrike-sdk = "0.0.1"\n')
  writeFixture(repoRoot, "python/pyproject.toml", `
[project]
name = "deepstrike"
version = "0.0.1"
`)
  writeJson(repoRoot, "crates/deepstrike-node/package.json", {
    name: "@deepstrike/core",
    version: "0.0.1",
    optionalDependencies: {},
  })
  writeJson(repoRoot, "crates/deepstrike-node/npm/linux-x64-gnu/package.json", {
    name: "@deepstrike/core-linux-x64-gnu",
    version: "0.0.1",
  })
  writeJson(repoRoot, "crates/deepstrike-node/npm/darwin-arm64/package.json", {
    name: "@deepstrike/core-darwin-arm64",
    version: "0.0.1",
  })
  writeJson(repoRoot, "node/package.json", {
    name: "@deepstrike/sdk",
    version: "0.0.1",
    dependencies: { "@deepstrike/core": "0.0.1" },
  })
  writeJson(repoRoot, "node/package-lock.json", {
    name: "@deepstrike/sdk",
    version: "0.0.1",
    packages: {
      "": {
        name: "@deepstrike/sdk",
        version: "0.0.1",
        dependencies: { "@deepstrike/core": "0.0.1" },
      },
      "node_modules/@deepstrike/core": {
        version: "0.0.1",
        resolved: "https://registry.npmjs.org/@deepstrike/core/-/core-0.0.1.tgz",
        optionalDependencies: {
          "@deepstrike/core-linux-x64-gnu": "0.0.1",
        },
      },
    },
  })
  writeJson(repoRoot, "wasm/package.json", {
    name: "@deepstrike/wasm",
    version: "0.0.1",
    dependencies: { "@deepstrike/wasm-kernel": "0.0.1" },
  })
  writeJson(repoRoot, "wasm/package-lock.json", {
    name: "@deepstrike/wasm",
    version: "0.0.1",
    packages: {
      "": {
        name: "@deepstrike/wasm",
        version: "0.0.1",
        dependencies: { "@deepstrike/wasm-kernel": "0.0.1" },
      },
    },
  })

  syncReleaseVersion({ repoRoot })

  assert.match(readText(repoRoot, "Cargo.toml"), /version = "1\.2\.3"/)
  assert.match(readText(repoRoot, "Cargo.toml"), /deepstrike-core = \{ path = "crates\/deepstrike-core", version = "1\.2\.3" \}/)
  assert.match(readText(repoRoot, "Cargo.lock"), /name = "deepstrike-wasm"\nversion = "1\.2\.3"/)
  assert.match(readText(repoRoot, "python/pyproject.toml"), /version = "1\.2\.3"/)
  assert.equal(readJson(repoRoot, "crates/deepstrike-node/package.json").version, "1.2.3")
  assert.deepEqual(
    readJson(repoRoot, "crates/deepstrike-node/package.json").optionalDependencies,
    {
      "@deepstrike/core-darwin-arm64": "1.2.3",
      "@deepstrike/core-linux-x64-gnu": "1.2.3",
    },
  )
  assert.equal(readJson(repoRoot, "node/package.json").dependencies["@deepstrike/core"], "1.2.3")
  assert.equal(readJson(repoRoot, "node/package-lock.json").packages[""].dependencies["@deepstrike/core"], "1.2.3")
  assert.equal(readJson(repoRoot, "node/package-lock.json").packages["node_modules/@deepstrike/core"].version, "1.2.3")
  assert.equal(readJson(repoRoot, "wasm/package.json").dependencies["@deepstrike/wasm-kernel"], "1.2.3")
  assert.equal(readJson(repoRoot, "wasm/package-lock.json").packages[""].dependencies["@deepstrike/wasm-kernel"], "1.2.3")
  assert.match(readText(repoRoot, "README.md"), /deepstrike-sdk = "1\.2\.3"/)
})

test("syncReleaseVersion check mode reports drift without mutating files", () => {
  const repoRoot = mkdtempSync(join(tmpdir(), "deepstrike-version-check-"))
  writeFixture(repoRoot, "VERSION", "1.2.3\n")
  writeFixture(repoRoot, "Cargo.toml", `
[workspace.package]
version = "0.0.1"

[workspace.dependencies]
deepstrike-core = { path = "crates/deepstrike-core", version = "0.0.1" }
deepstrike-tokenizer = { path = "crates/deepstrike-tokenizer", version = "0.0.1" }
`)
  writeFixture(repoRoot, "Cargo.lock", `
[[package]]
name = "deepstrike-core"
version = "0.0.1"

[[package]]
name = "deepstrike-node"
version = "0.0.1"

[[package]]
name = "deepstrike-py"
version = "0.0.1"

[[package]]
name = "deepstrike-sdk"
version = "0.0.1"

[[package]]
name = "deepstrike-tokenizer"
version = "0.0.1"

[[package]]
name = "deepstrike-wasm"
version = "0.0.1"
`)
  writeFixture(repoRoot, "README.md", 'deepstrike-sdk = "0.0.1"\n')
  writeFixture(repoRoot, "python/pyproject.toml", '[project]\nversion = "0.0.1"\n')
  writeJson(repoRoot, "crates/deepstrike-node/package.json", {
    name: "@deepstrike/core",
    version: "0.0.1",
    optionalDependencies: {},
  })
  writeJson(repoRoot, "crates/deepstrike-node/npm/linux-x64-gnu/package.json", {
    name: "@deepstrike/core-linux-x64-gnu",
    version: "0.0.1",
  })
  writeJson(repoRoot, "node/package.json", {
    name: "@deepstrike/sdk",
    version: "0.0.1",
    dependencies: { "@deepstrike/core": "0.0.1" },
  })
  writeJson(repoRoot, "node/package-lock.json", {
    name: "@deepstrike/sdk",
    version: "0.0.1",
    packages: { "": { version: "0.0.1", dependencies: { "@deepstrike/core": "0.0.1" } } },
  })
  writeJson(repoRoot, "wasm/package.json", {
    name: "@deepstrike/wasm",
    version: "0.0.1",
    dependencies: { "@deepstrike/wasm-kernel": "0.0.1" },
  })
  writeJson(repoRoot, "wasm/package-lock.json", {
    name: "@deepstrike/wasm",
    version: "0.0.1",
    packages: { "": { version: "0.0.1", dependencies: { "@deepstrike/wasm-kernel": "0.0.1" } } },
  })

  assert.throws(
    () => syncReleaseVersion({ repoRoot, check: true }),
    /Release version drift detected/,
  )
  assert.match(readText(repoRoot, "Cargo.toml"), /version = "0\.0\.1"/)
})

function writeFixture(repoRoot, relativePath, content) {
  const path = join(repoRoot, relativePath)
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, content.trimStart())
}

function writeJson(repoRoot, relativePath, value) {
  writeFixture(repoRoot, relativePath, `${JSON.stringify(value, null, 2)}\n`)
}

function readText(repoRoot, relativePath) {
  return readFileSync(join(repoRoot, relativePath), "utf8")
}

function readJson(repoRoot, relativePath) {
  return JSON.parse(readText(repoRoot, relativePath))
}
