# Release Runbook

DeepStrike publishes four release families from one source tree:

| Release family | Published packages |
|----------------|--------------------|
| Rust | `deepstrike-tokenizer`, `deepstrike-core`, `deepstrike-sdk` |
| Python | `deepstrike` |
| Node.js | `@deepstrike/core-*`, `@deepstrike/core`, `@deepstrike/sdk` |
| WASM | `@deepstrike/wasm-kernel`, `@deepstrike/wasm` |

All release families share one canonical version:

```text
VERSION
```

Do not edit package versions by hand. Change `VERSION`, then let the sync script propagate it into every derived manifest.

---

## Version propagation

```bash
# 1. Edit the one true source
printf '0.1.10\n' > VERSION

# 2. Fan the version out to all derived release files
node scripts/sync-release-version.mjs

# 3. Prove the repository is synchronized
node scripts/sync-release-version.mjs --check
```

`scripts/sync-release-version.mjs` updates:

- Rust workspace metadata in `Cargo.toml`
- Workspace package entries in `Cargo.lock`
- Python package metadata in `python/pyproject.toml`
- Node core/platform/sdk manifests and `node/package-lock.json`
- WASM sdk manifest and `wasm/package-lock.json`
- the Rust dependency example in `README.md`

Release workflows run the same check in CI. A tag may trigger publication, but it is **not** the version source; `vX.Y.Z` must match `VERSION`.

---

## Pre-release checklist

Run these from the repository root:

```bash
node scripts/sync-release-version.mjs --check
node --test scripts/release-version.test.mjs
cargo metadata --format-version=1 --no-deps >/dev/null
npm test --prefix node
npm test --prefix wasm
```

Before publishing, also confirm:

- the working tree is clean
- changelog / release notes are ready
- required registry credentials are configured in GitHub Actions
- the target version has not already been published

---

## Standard release flow

```bash
# 1. Set the next version
printf '0.1.10\n' > VERSION
node scripts/sync-release-version.mjs

# 2. Verify locally
node scripts/sync-release-version.mjs --check
node --test scripts/release-version.test.mjs
cargo metadata --format-version=1 --no-deps >/dev/null
npm test --prefix node
npm test --prefix wasm

# 3. Commit the release preparation
git add VERSION Cargo.toml Cargo.lock README.md node/package-lock.json wasm/package.json wasm/package-lock.json
git add python/pyproject.toml crates/deepstrike-node/package.json crates/deepstrike-node/npm
git commit -m "chore: bump version to 0.1.10"

# 4. Push the commit and matching tag
git push origin main
git tag v0.1.10
git push origin v0.1.10
```

Pushing the tag starts:

- `.github/workflows/release-rust.yml`
- `.github/workflows/release-python.yml`
- `.github/workflows/release-node.yml`
- `.github/workflows/release-wasm.yml`

Each workflow first checks that:

1. the tag matches `VERSION`
2. the derived release files are synchronized

Only then does publication begin.

---

## Node.js release notes

The Node release has three layers:

```text
platform packages  →  @deepstrike/core  →  @deepstrike/sdk
```

Publish order matters:

1. `@deepstrike/core-<platform>` binary packages
2. `@deepstrike/core` loader package
3. `@deepstrike/sdk`

The loader accepts two native-addon naming shapes:

| Shape | Where it appears |
|-------|------------------|
| `deepstrike-core.<platform>.node` | published platform packages |
| `index.<platform>.node` | local `napi build --platform` artifacts used in CI |

This distinction is intentional. If Node tests fail with “Failed to load `@deepstrike/core-...`”, first check whether the local build artifact exists and whether the loader is seeing the right naming shape.

---

## Recovery paths

### Tag does not match `VERSION`

The release workflow stops before publishing. Fix one side of the mismatch:

- if the version is wrong, update `VERSION`, rerun the sync script, commit, and retag
- if the tag is wrong, delete the incorrect tag and create the matching one

### `sync-release-version --check` fails

Run:

```bash
node scripts/sync-release-version.mjs
git diff
```

Commit the generated updates before retrying the release.

### A package version already exists in a registry

`scripts/publish-npm-package.mjs` skips npm packages that are already published. Rust/Python registries do not permit overwriting a published version. Bump `VERSION`, sync, verify, and release a new version.

### Only one release family needs rerunning

- Node supports manual dispatch through `release-node.yml`
- Other workflows are tag-driven; prefer fixing the issue and publishing a new version rather than trying to mutate an already-published release

---

## Files that are intentionally not synchronized

Example applications and integration fixtures are consumers, not release artifacts. Their dependency ranges may intentionally lag or demonstrate older install patterns:

- `example/`
- `tests/node/`

Do not fold them into release-version propagation unless their role changes.
