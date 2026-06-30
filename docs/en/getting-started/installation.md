# Installation

## Python

```bash
cd python
pip install -e .
# or
pip install deepstrike
```

Environment variables (choose based on your provider):

```bash
export ANTHROPIC_API_KEY=sk-...
# export OPENAI_API_KEY=sk-...
```

## Node.js / TypeScript

```bash
npm install @deepstrike/sdk
```

## Rust

```toml
[dependencies]
deepstrike-sdk = "0.2"
```

## WASM

```bash
npm install @deepstrike/wasm
```

## Preview Docs Locally

```bash
npm ci
npm run docs:dev    # http://localhost:5173
npm run docs:build  # static build → GitHub Pages
```

## Verify Installation

```bash
cd python
python -c "import deepstrike; print(deepstrike.__version__)"
```

## Next Steps

[Hello Agent →](./hello-agent)
