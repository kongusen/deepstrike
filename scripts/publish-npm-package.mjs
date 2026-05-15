import { execFileSync } from "node:child_process"
import { readFileSync } from "node:fs"
import { resolve } from "node:path"

const packageDir = resolve(process.argv[2] ?? ".")
const pkg = JSON.parse(readFileSync(resolve(packageDir, "package.json"), "utf8"))
const spec = `${pkg.name}@${pkg.version}`

try {
  execFileSync("npm", ["view", spec, "version"], { stdio: "ignore" })
  console.log(`Skipping ${spec}: already published`)
} catch {
  execFileSync("npm", ["publish", packageDir, "--access", "public"], { stdio: "inherit" })
}
