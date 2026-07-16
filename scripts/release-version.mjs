import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const semverPattern = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/
const cargoWorkspacePackages = [
  "deepstrike-core",
  // deepstrike-lab is version.workspace=true: without a lock rewrite here, every release
  // leaves its Cargo.lock entry one version behind (dirty tree after the next local cargo run).
  "deepstrike-lab",
  "deepstrike-node",
  "deepstrike-py",
  "deepstrike-sdk",
  "deepstrike-wasm",
]

export function readCanonicalVersion(repoRoot) {
  const version = readFileSync(join(repoRoot, "VERSION"), "utf8").trim()
  if (!semverPattern.test(version)) {
    throw new Error(`Invalid canonical version in VERSION: ${version || "<empty>"}`)
  }
  return version
}

export function syncReleaseVersion({ repoRoot, check = false }) {
  const version = readCanonicalVersion(repoRoot)
  const platformPackagePaths = listPlatformPackagePaths(repoRoot)
  const platformNames = platformPackagePaths
    .map(path => readJson(path).name)
    .sort()

  const updates = [
    updateTextFile(join(repoRoot, "Cargo.toml"), text => updateCargoToml(text, version)),
    updateTextFile(join(repoRoot, "Cargo.lock"), text => updateCargoLock(text, version)),
    updateTextFile(join(repoRoot, "python", "pyproject.toml"), text => updatePythonProject(text, version)),
    updateTextFile(join(repoRoot, "README.md"), text => updateReadme(text, version)),
    updateJsonFile(join(repoRoot, "crates", "deepstrike-node", "package.json"), pkg => ({
      ...pkg,
      version,
      optionalDependencies: Object.fromEntries(platformNames.map(name => [name, version])),
    })),
    ...platformPackagePaths.map(path => updateJsonFile(path, pkg => ({ ...pkg, version }))),
    updateJsonFile(join(repoRoot, "node", "package.json"), pkg => ({
      ...pkg,
      version,
      dependencies: {
        ...pkg.dependencies,
        "@deepstrike/core": version,
      },
    })),
    updateJsonFile(join(repoRoot, "node", "package-lock.json"), lock => updateNodeLock(lock, version, platformNames)),
    updateJsonFile(join(repoRoot, "wasm", "package.json"), pkg => ({
      ...pkg,
      version,
      dependencies: {
        ...pkg.dependencies,
        "@deepstrike/wasm-kernel": version,
      },
    })),
    updateJsonFile(join(repoRoot, "wasm", "package-lock.json"), lock => updateWasmLock(lock, version)),
  ]

  const changed = updates.filter(update => update.before !== update.after)
  if (check && changed.length > 0) {
    const files = changed.map(update => update.path).join(", ")
    throw new Error(`Release version drift detected for ${version}: ${files}`)
  }

  if (!check) {
    for (const update of changed) {
      writeFileSync(update.path, update.after)
    }
  }

  return { version, changedFiles: changed.map(update => update.path) }
}

function listPlatformPackagePaths(repoRoot) {
  const platformRoot = join(repoRoot, "crates", "deepstrike-node", "npm")
  if (!existsSync(platformRoot)) return []

  return readdirSync(platformRoot, { withFileTypes: true })
    .filter(entry => entry.isDirectory())
    .map(entry => join(platformRoot, entry.name, "package.json"))
    .filter(path => existsSync(path))
    .sort()
}

function updateTextFile(path, transform) {
  const before = readFileSync(path, "utf8")
  return { path, before, after: transform(before) }
}

function updateJsonFile(path, transform) {
  const before = readFileSync(path, "utf8")
  const after = `${JSON.stringify(transform(JSON.parse(before)), null, 2)}\n`
  return { path, before, after }
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"))
}

function updateCargoToml(text, version) {
  let next = replaceRequired(
    text,
    /(\[workspace\.package\][\s\S]*?\nversion = ")[^"]+(")/,
    `$1${version}$2`,
    "Cargo.toml workspace.package.version",
  )
  next = replaceRequired(
    next,
    /(deepstrike-core = \{ path = "crates\/deepstrike-core", version = ")[^"]+(" \})/,
    `$1${version}$2`,
    "Cargo.toml deepstrike-core workspace dependency",
  )
  const tokenizerPattern = /(deepstrike-tokenizer = \{ path = "crates\/deepstrike-tokenizer", version = ")[^"]+(" \})/
  if (tokenizerPattern.test(next)) {
    next = next.replace(tokenizerPattern, `$1${version}$2`)
  }
  return next
}

function updateCargoLock(text, version) {
  return cargoWorkspacePackages.reduce(
    (nextText, packageName) =>
      replaceRequired(
        nextText,
        new RegExp(`(name = "${escapeRegExp(packageName)}"\\r?\\nversion = ")[^"]+(")`),
        `$1${version}$2`,
        `Cargo.lock package ${packageName}`,
      ),
    text,
  )
}

function updatePythonProject(text, version) {
  return replaceRequired(
    text,
    /(\[project\][\s\S]*?\nversion = ")[^"]+(")/,
    `$1${version}$2`,
    "python/pyproject.toml project.version",
  )
}

function updateReadme(text, version) {
  let next = replaceRequired(
    text,
    /(deepstrike-sdk = ")[^"]+(")/,
    `$1${version}$2`,
    "README.md deepstrike-sdk example version",
  )
  next = next.replace(/Version \*\*[^*]+\*\*/, `Version **${version}**`)
  next = next.replace(/npm install @deepstrike\/sdk@[^\s\n]+/, `npm install @deepstrike/sdk@${version}`)
  next = next.replace(/pip install deepstrike==[^\s\n]+/, `pip install deepstrike==${version}`)
  next = next.replace(/## Kernel \(v[^)]+\)/, `## Kernel (v${version})`)
  return next
}

function updateNodeLock(lock, version, platformNames) {
  const next = {
    ...lock,
    version,
    packages: {
      ...lock.packages,
      "": {
        ...lock.packages[""],
        version,
        dependencies: {
          ...lock.packages[""].dependencies,
          "@deepstrike/core": version,
        },
      },
    },
  }

  if (lock.packages["node_modules/@deepstrike/core"]) {
    next.packages["node_modules/@deepstrike/core"] = {
      ...lock.packages["node_modules/@deepstrike/core"],
      version,
      resolved: `https://registry.npmjs.org/@deepstrike/core/-/core-${version}.tgz`,
      optionalDependencies: Object.fromEntries(platformNames.map(name => [name, version])),
    }
  }

  return next
}

function updateWasmLock(lock, version) {
  return {
    ...lock,
    version,
    packages: {
      ...lock.packages,
      "": {
        ...lock.packages[""],
        version,
        dependencies: {
          ...lock.packages[""].dependencies,
          "@deepstrike/wasm-kernel": version,
        },
      },
    },
  }
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

function replaceRequired(text, pattern, replacement, label) {
  if (!pattern.test(text)) {
    throw new Error(`Missing expected version field: ${label}`)
  }
  return text.replace(pattern, replacement)
}
