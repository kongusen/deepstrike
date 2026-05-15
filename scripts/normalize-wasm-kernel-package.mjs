import { readFileSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"

const version = process.argv[2]
const packageDir = resolve(process.argv[3] ?? "crates/deepstrike-wasm/pkg")
const packagePath = resolve(packageDir, "package.json")

if (!version) {
  console.error("Usage: node scripts/normalize-wasm-kernel-package.mjs <version> [package-dir]")
  process.exit(1)
}

const pkg = JSON.parse(readFileSync(packagePath, "utf8"))
pkg.name = "@deepstrike/wasm-kernel"
pkg.version = version
writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`)
