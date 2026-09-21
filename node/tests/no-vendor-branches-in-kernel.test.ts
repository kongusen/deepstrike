import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative, sep } from "node:path"

/** spc_007-05: static guard against `if (provider === "openai")`-style vendor branches leaking
 *  into Kernel-facing code (spc_007 §7's third acceptance criterion; spc_001 §4's "otherwise it
 *  degrades into `if provider == openai ...`" warning). Scans the Rust kernel crate and the Node
 *  SDK's `src/`; provider branching belongs only inside explicit adapters. */
const VENDOR_BRANCH_PATTERN = /\bprovider\s*={2,3}\s*['"]/

function collectSourceFiles(dir: string, exclude: (absolutePath: string) => boolean, extensions: RegExp): string[] {
  const files: string[] = []
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    if (exclude(full)) continue
    const stat = statSync(full)
    if (stat.isDirectory()) files.push(...collectSourceFiles(full, exclude, extensions))
    else if (extensions.test(entry)) files.push(full)
  }
  return files
}

function findVendorBranchViolations(repoRoot: string): string[] {
  const nodeExclude = (full: string) => full.includes(`${sep}node_modules${sep}`)
  const rustExclude = (full: string) => full.includes(`${sep}target${sep}`)

  const files = [
    ...collectSourceFiles(join(repoRoot, "crates", "deepstrike-core", "src"), rustExclude, /\.rs$/),
    ...collectSourceFiles(join(repoRoot, "node", "src"), nodeExclude, /\.ts$/),
  ]

  const violations: string[] = []
  for (const file of files) {
    if (VENDOR_BRANCH_PATTERN.test(readFileSync(file, "utf8"))) {
      violations.push(relative(repoRoot, file))
    }
  }
  return violations
}

describe("spc_007-05: no vendor branches in Kernel-facing code", () => {
  it("crates/deepstrike-core/src and node/src contain no provider === vendor branches", () => {
    const repoRoot = join(process.cwd(), "..")
    expect(findVendorBranchViolations(repoRoot)).toEqual([])
  })
})
