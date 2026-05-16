import { readFileSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { readCanonicalVersion } from "./release-version.mjs"

const repoRoot = fileURLToPath(new URL("..", import.meta.url))
const version = process.argv[2] ?? readCanonicalVersion(repoRoot)
const packageDir = resolve(process.argv[3] ?? "crates/deepstrike-wasm/pkg")
const packagePath = resolve(packageDir, "package.json")

const pkg = JSON.parse(readFileSync(packagePath, "utf8"))
pkg.name = "@deepstrike/wasm-kernel"
pkg.version = version
writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`)
