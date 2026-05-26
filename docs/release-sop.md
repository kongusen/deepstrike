# Deepstrike 发布 SOP

> 适用版本：v0.1.11+  
> 每次发布覆盖四条产物链：**Rust crates → crates.io**、**Python wheels → PyPI**、**Node.js native addon → npm**、**WASM SDK → npm**

---

## 发布前检查（每次必做）

### 1. 确认 CI 绿

在 GitHub Actions → **CI** workflow 确认 `main` 分支最新 commit 所有 job 通过：

- `rust` — cargo test
- `node` — native build + npm test + musl 验证
- `node-linux-arm64-musl` — ARM64 musl 验证
- `wasm` — wasm-pack build + tsc
- `python` — maturin build + pytest

**CI 未全绿不得发布。**

### 2. 确认本地环境

```bash
# 在 repo 根目录执行
git status          # 必须干净，无未提交文件
git log origin/main..HEAD  # 应无输出（HEAD 已推送）
node --version      # >= 20
cargo --version
python --version    # >= 3.12
```

### 3. 确认 secrets 已配置

| Secret | 用途 |
|--------|------|
| `CARGO_REGISTRY_TOKEN` | crates.io 发布 |
| `NPM_TOKEN` | npm 发布（@deepstrike/core、@deepstrike/sdk、@deepstrike/wasm） |
| PyPI 采用 OIDC trusted publishing，无需 token | — |

---

## 发布流程

### Step 1 — 执行发布脚本

```bash
./scripts/release.sh <新版本号>
# 示例：
./scripts/release.sh 0.1.12
```

脚本会自动完成：

1. 验证工作区干净 + HEAD 在 origin/main 上
2. 写入 `VERSION` 文件
3. 调用 `sync-release-version.mjs` 同步以下文件版本号：

| 文件 | 字段 |
|------|------|
| `Cargo.toml` | `[workspace.package] version` + workspace deps |
| `Cargo.lock` | 6 个 deepstrike-* crate |
| `python/pyproject.toml` | `[project] version` |
| `README.md` | Cargo 示例版本 |
| `crates/deepstrike-node/package.json` | version + optionalDependencies |
| `crates/deepstrike-node/npm/*/package.json` | version（7个平台包） |
| `node/package.json` + `package-lock.json` | version + @deepstrike/core dep |
| `wasm/package.json` + `wasm/package-lock.json` | version + @deepstrike/wasm-kernel dep |

4. 二次校验所有文件无漂移（`--check`）
5. `git commit -m "chore: release v<version>"`（如已是最新版本则跳过）
6. `git tag v<version>`（已存在则报错，防止误操作）

### Step 2 — 推送

```bash
git push origin main && git push origin v<version>

# 代理环境：
https_proxy=http://127.0.0.1:7897 http_proxy=http://127.0.0.1:7897 \
  git push origin main && git push origin v<version>
```

### Step 3 — 监控 GitHub Actions

tag 推送后，以下四个 workflow **并行**触发，每个都有前置 `verify` job 做版本一致性检查：

```
v<version> push
├── Release Rust Crates      (~5 min)   → crates.io
│   ├── verify
│   └── publish
│       ├── dry-run (3 crates)
│       ├── publish deepstrike-tokenizer
│       ├── wait for index (search 重试，最多 3 min)
│       ├── publish deepstrike-core
│       ├── wait for index
│       └── publish deepstrike-sdk
│
├── Release Python SDK       (~25 min)  → PyPI
│   ├── verify
│   ├── build-wheels (8 平台并行)
│   │   └── smoke install (Linux x64 / macOS arm64 / Win x64)
│   ├── build-sdist
│   │   └── smoke install from sdist
│   └── publish
│       ├── twine check (验证 wheel/sdist metadata)
│       └── pypa/gh-action-pypi-publish
│
├── Release Node.js SDK      (~20 min)  → npm
│   ├── verify
│   ├── build-native (7 平台并行)
│   └── publish
│       ├── validate npm package layout
│       ├── smoke install linux-x64-gnu
│       ├── publish 7 个 @deepstrike/core-<platform>
│       ├── publish @deepstrike/core
│       ├── build @deepstrike/sdk
│       └── publish @deepstrike/sdk
│
└── Release WASM SDK         (~10 min)  → npm
    ├── verify
    └── publish
        ├── wasm-pack build
        ├── npm run build + npm test
        ├── smoke install from packed tgz
        ├── publish @deepstrike/wasm-kernel
        └── publish @deepstrike/wasm
```

> 任意 job 失败 → 停止等待，查看日志，参考下方「失败处理」。

### Step 4 — 验证发布结果

所有 workflow 变绿后，执行以下验证：

**Rust**
```bash
cargo add deepstrike-sdk@<version> --dry-run
# 或直接 search：
cargo search deepstrike-core | head -3
```

**Python**
```bash
pip install deepstrike==<version>
python -c "import deepstrike; print(deepstrike.__version__)"
```

**Node.js**
```bash
npm view @deepstrike/sdk@<version> version
npm install @deepstrike/sdk@<version>
node -e "const s = require('@deepstrike/sdk'); console.log('ok')"
```

**WASM**
```bash
npm view @deepstrike/wasm@<version> version
```

---

## 失败处理

### verify job 失败

`Release version drift` 错误 — 本地执行：
```bash
node scripts/sync-release-version.mjs
node scripts/sync-release-version.mjs --check
git diff  # 确认哪些文件未对齐
```
修复后重新走 Step 1 流程（重打 tag 需先 `git tag -d v<version>`）。

### Rust dry-run 失败

`cargo publish --dry-run` 报错 — 常见原因：

| 错误 | 处理 |
|------|------|
| `crate too large` | 检查 `.cargo/` 缓存是否被打包 |
| `missing field description` | 检查 `Cargo.toml` 元数据 |
| 依赖版本未在 crates.io 索引 | 等待上游 crate 发布，或用 `--registry local` 测试 |

### Rust index 等待超时

`cargo search` 重试 12 次（3 分钟）仍未找到 → crates.io 可能有延迟。手动等待后到 Actions → 点击失败的 job → **Re-run failed jobs**。

### Python wheel smoke 失败

只有 3/8 平台运行 smoke（其余交叉编译，无法在 runner 上安装）。smoke 失败说明 PyO3 FFI 层有问题，检查：
```bash
cd python && maturin build --release && pip install dist/*.whl
python -c "from deepstrike.kernel import KernelRuntime, LoopPolicy; KernelRuntime(LoopPolicy())"
```

### npm publish 报 `403 Forbidden`

`NPM_TOKEN` 过期或权限不足：
- 登录 npmjs.com → Access Tokens → 生成新 `Automation` 类型 token
- 更新 GitHub repo Settings → Secrets → `NPM_TOKEN`
- Re-run failed jobs

### WASM smoke 失败

检查 `normalize-wasm-kernel-package.mjs` 生成的 `package.json` 是否正确：
```bash
node scripts/normalize-wasm-kernel-package.mjs "$(cat VERSION)" crates/deepstrike-wasm/pkg
cat crates/deepstrike-wasm/pkg/package.json | grep version
```

---

## 版本号规范

遵循 [Semantic Versioning 2.0](https://semver.org/)：

| 类型 | 版本号变化 | 示例 |
|------|-----------|------|
| Breaking change（API 不兼容） | major 递增 | 0.1.x → 1.0.0 |
| 新功能（向后兼容） | minor 递增 | 0.1.11 → 0.1.12 |
| Bug fix | patch 递增 | 0.1.11 → 0.1.11 |
| 预发布 | pre-release 后缀 | 0.1.12-beta.1 |

`VERSION` 文件是唯一真相源，所有 manifest 由 `scripts/sync-release-version.mjs` 派生。**不要手动修改其他文件的版本号。**

---

## 快速参考

```bash
# 完整发布（假设 CI 已绿、工作区干净）
./scripts/release.sh 0.1.12
git push origin main && git push origin v0.1.12

# 验证版本对齐（不写文件）
node scripts/sync-release-version.mjs --check

# 查看当前 canonical 版本
cat VERSION
```

---

## 相关文件

| 文件 | 作用 |
|------|------|
| `VERSION` | 唯一版本真相源 |
| `scripts/release.sh` | 本地发布入口 |
| `scripts/release-version.mjs` | 版本同步核心逻辑 |
| `scripts/sync-release-version.mjs` | CLI 封装（check/write 两用） |
| `scripts/publish-npm-package.mjs` | npm 幂等发布（已发布则跳过） |
| `scripts/normalize-wasm-kernel-package.mjs` | wasm-pack 产物 package.json 修正 |
| `.github/workflows/_verify-version.yml` | Reusable 版本校验 workflow |
| `.github/workflows/release-rust.yml` | crates.io 发布 |
| `.github/workflows/release-python.yml` | PyPI 发布 |
| `.github/workflows/release-node.yml` | npm Node.js 发布 |
| `.github/workflows/release-wasm.yml` | npm WASM 发布 |
