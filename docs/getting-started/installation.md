# 安装

## Python

```bash
cd python
pip install -e .
# 或
pip install deepstrike
```

环境变量（按 provider 选择）：

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

## 文档本地预览

```bash
npm ci
npm run docs:dev    # http://localhost:5173
npm run docs:build  # 静态构建 → GitHub Pages
```

## 验证安装

```bash
cd python
python -c "import deepstrike; print(deepstrike.__version__)"
```

## 下一步

[Hello Agent →](./hello-agent)
