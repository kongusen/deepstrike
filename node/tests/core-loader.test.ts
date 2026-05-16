import { execFileSync } from "node:child_process"
import { copyFileSync, mkdtempSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

function localBuildTriple(): string {
  if (process.platform === "darwin") return `darwin-${process.arch}`
  if (process.platform === "win32") return "win32-x64-msvc"
  if (process.platform === "linux") {
    const report = typeof process.report?.getReport === "function"
      ? process.report.getReport()
      : null
    const libc = report?.header?.glibcVersionRuntime ? "gnu" : "musl"
    return `linux-${process.arch}-${libc}`
  }

  throw new Error(`Unsupported test platform: ${process.platform}/${process.arch}`)
}

describe("@deepstrike/core loader", () => {
  it("loads napi local-build artifacts before requiring published platform packages", () => {
    const triple = localBuildTriple()
    const tempDir = mkdtempSync(join(tmpdir(), "deepstrike-core-loader-"))
    const sourceLoader = join(process.cwd(), "..", "crates", "deepstrike-node", "index.js")
    const tempLoader = join(tempDir, "index.js")
    const localArtifact = join(tempDir, `index.${triple}.node`)

    copyFileSync(sourceLoader, tempLoader)
    writeFileSync(localArtifact, 'module.exports = { source: "local-build" }\n')

    const output = execFileSync(
      process.execPath,
      [
        "-e",
        [
          "const Module = require('module')",
          "Module._extensions['.node'] = Module._extensions['.js']",
          `const loaded = require(${JSON.stringify(tempLoader)})`,
          "process.stdout.write(loaded.source)",
        ].join(";"),
      ],
      { encoding: "utf8" },
    )

    expect(output).toBe("local-build")
  })
})
