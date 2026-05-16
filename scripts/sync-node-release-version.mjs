import { fileURLToPath } from "node:url"
import { syncReleaseVersion } from "./release-version.mjs"

if (process.argv.length > 2) {
  console.error("Version arguments are no longer accepted. Edit VERSION, then run scripts/sync-release-version.mjs.")
  process.exit(1)
}

const repoRoot = fileURLToPath(new URL("..", import.meta.url))
const { version, changedFiles } = syncReleaseVersion({ repoRoot })
console.log(
  changedFiles.length === 0
    ? `Release versions already synchronized at ${version}`
    : `Synchronized release version ${version} across ${changedFiles.length} files`,
)
