#!/usr/bin/env bash
# scripts/release.sh <version>
#
# One-command release: syncs all platform SDK versions, commits, and tags.
# Version is taken from the argument (e.g. 0.1.12 or v0.1.12).
#
# Usage:
#   ./scripts/release.sh 0.1.12
#   ./scripts/release.sh v0.1.12
#
# After this script, push with:
#   git push origin main && git push origin v<version>
#   (or set HTTP_PROXY / HTTPS_PROXY if behind a proxy)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# ── 1. Resolve version ────────────────────────────────────────────────────────
VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  echo "Usage: $0 <version>  (e.g. 0.1.12 or v0.1.12)" >&2
  exit 1
fi
VERSION="${VERSION#v}"   # strip leading 'v' if present

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid semver: $VERSION" >&2
  exit 1
fi

echo "==> Releasing v${VERSION}"

# ── 2. Pre-flight: working tree must be clean ─────────────────────────────────
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Error: working tree has uncommitted changes. Commit or stash before releasing." >&2
  git status --short >&2
  exit 1
fi
echo "    Working tree clean ✓"

# ── 3. Pre-flight: HEAD must be on origin/main ────────────────────────────────
git fetch origin main --quiet
if ! git merge-base --is-ancestor HEAD origin/main 2>/dev/null; then
  echo "Error: HEAD is not an ancestor of origin/main." >&2
  echo "       Push your commits first, or verify you are on the right branch." >&2
  exit 1
fi
echo "    HEAD is on origin/main ✓"

# ── 4. Write canonical VERSION file ──────────────────────────────────────────
echo "$VERSION" > VERSION
echo "    VERSION file → $VERSION"

# ── 5. Sync all platform packages via the existing JS script ─────────────────
echo "    Syncing package versions..."
node scripts/sync-release-version.mjs

# ── 6. Verify — fail fast if any file still drifts ───────────────────────────
echo "    Verifying..."
node scripts/sync-release-version.mjs --check
echo "    All versions aligned at $VERSION ✓"

# ── 7. Stage every version-bearing file ──────────────────────────────────────
git add \
  VERSION \
  Cargo.toml \
  Cargo.lock \
  python/pyproject.toml \
  README.md \
  crates/deepstrike-node/package.json \
  crates/deepstrike-node/npm/*/package.json \
  node/package.json \
  node/package-lock.json \
  wasm/package.json \
  wasm/package-lock.json

# Only commit if there is something to commit
if git diff --cached --quiet; then
  echo "    Nothing changed — versions were already at $VERSION"
else
  git commit -m "chore: release v${VERSION}"
  echo "    Committed version bump"
fi

# ── 8. Create tag (no -f: fail if tag already exists to prevent accidental re-tag) ──
if git rev-parse "v${VERSION}" >/dev/null 2>&1; then
  echo "Error: tag v${VERSION} already exists. Delete it first if you intend to re-release." >&2
  exit 1
fi
git tag "v${VERSION}"
echo "    Tagged v${VERSION} → $(git rev-parse --short HEAD)"

# ── 9. Print push instructions ────────────────────────────────────────────────
echo ""
echo "Done. Push with:"
echo ""
echo "  git push origin main && git push origin v${VERSION}"
echo ""
echo "Behind a proxy? Prefix with:"
echo "  https_proxy=http://127.0.0.1:7897 http_proxy=http://127.0.0.1:7897 \\"
