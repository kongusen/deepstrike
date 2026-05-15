import { readdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const version = process.argv[2]

if (!version || !/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) {
  console.error("Usage: node scripts/sync-node-release-version.mjs <semver>")
  process.exit(1)
}

const repoRoot = new URL("..", import.meta.url).pathname
const coreDir = join(repoRoot, "crates", "deepstrike-node")
const platformRoot = join(coreDir, "npm")
const sdkDir = join(repoRoot, "node")

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"))
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`)
}

const platformPackages = readdirSync(platformRoot, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => join(platformRoot, entry.name, "package.json"))
  .sort()

const platformNames = []

for (const packagePath of platformPackages) {
  const pkg = readJson(packagePath)
  pkg.version = version
  platformNames.push(pkg.name)
  writeJson(packagePath, pkg)
}

const corePackagePath = join(coreDir, "package.json")
const corePackage = readJson(corePackagePath)
corePackage.version = version
corePackage.optionalDependencies = Object.fromEntries(
  platformNames.sort().map((name) => [name, version]),
)
writeJson(corePackagePath, corePackage)

const sdkPackagePath = join(sdkDir, "package.json")
const sdkPackage = readJson(sdkPackagePath)
sdkPackage.version = version
sdkPackage.dependencies["@deepstrike/core"] = version
writeJson(sdkPackagePath, sdkPackage)
