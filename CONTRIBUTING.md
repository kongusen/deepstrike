# Contributing to DeepStrike

Thanks for helping improve DeepStrike. This project has a small public surface and a strict runtime boundary: the kernel owns agent semantics, while SDKs own host effects. Contributions should preserve that split.

## Good First Contributions

- Fix or expand SDK examples.
- Improve provider documentation.
- Add focused tests for runtime edge cases.
- Tighten docs around kernel ABI behavior, context compression, governance, or release flow.

Open an issue before starting a large feature, public API change, or architecture change.

## Development Setup

Requirements:

- Rust 1.85+
- Node.js 18+
- Python 3.10+

```bash
git clone https://github.com/kongusen/deepstrike.git
cd deepstrike

cargo build
cargo test

cd node
npm install
npm run build
npm test
```

For Python:

```bash
cd python
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pytest-asyncio
maturin develop --release
pytest
```

For WASM:

```bash
cd wasm
npm install
npm run build
npm test
```

## Pull Request Checklist

- Keep the kernel / host boundary intact.
- Add or update tests for behavior changes.
- Update docs when public APIs, provider behavior, runtime semantics, or release steps change.
- Keep generated files and dependency updates scoped to the change.
- Include a clear PR description with motivation, approach, and verification commands.

## Commit Style

Use concise conventional-style subjects:

```text
feat: add provider profile override
fix: preserve compression log during renewal
docs: refresh SDK quick start
test: cover milestone wake recovery
```

## Documentation Changes

Docs should explain what a user can do, what contract the runtime guarantees, and what tradeoff led to the design. Avoid duplicating implementation details that are already obvious from source.

## Community

For design discussion and project questions, join Discord: <https://discord.gg/cwS3RBYCv>.
