import { fileURLToPath } from "node:url"
import { syncReleaseVersion } from "./release-version.mjs"

const repoRoot = fileURLToPath(new URL("..", import.meta.url))
const check = process.argv.includes("--check")

try {
  const { version, changedFiles } = syncReleaseVersion({ repoRoot, check })
  if (check) {
    console.log(`Release versions are synchronized at ${version}`)
  } else if (changedFiles.length === 0) {
    console.log(`Release versions already synchronized at ${version}`)
  } else {
    console.log(`Synchronized release version ${version} across ${changedFiles.length} files`)
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error))
  process.exit(1)
}
