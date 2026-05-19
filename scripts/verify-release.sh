#!/usr/bin/env bash
# Run pre-release checks from anywhere in the repo.
#
# Usage (from any directory):
#   ./scripts/verify-release.sh
#   ./scripts/verify-release.sh --skip-rust   # node + wasm only
#   ./scripts/verify-release.sh --skip-npm    # rust only

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

SKIP_RUST=false
SKIP_NPM=false
for arg in "$@"; do
  case "$arg" in
    --skip-rust) SKIP_RUST=true ;;
    --skip-npm) SKIP_NPM=true ;;
    -h|--help)
      echo "Usage: $0 [--skip-rust] [--skip-npm]"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 1
      ;;
  esac
done

echo "==> Pre-release verify (repo root: $REPO_ROOT)"
echo ""

echo "==> Version sync"
node scripts/sync-release-version.mjs --check
node --test scripts/release-version.test.mjs
echo ""

if [[ "$SKIP_RUST" == false ]]; then
  echo "==> Rust"
  cargo metadata --format-version=1 --no-deps >/dev/null
  cargo test -p deepstrike-tests
  echo ""
fi

if [[ "$SKIP_NPM" == false ]]; then
  echo "==> Node SDK"
  npm test --prefix "$REPO_ROOT/node"
  echo ""
  echo "==> WASM SDK"
  npm test --prefix "$REPO_ROOT/wasm"
  echo ""
fi

echo "==> All checks passed"
