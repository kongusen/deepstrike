# GitHub Wiki 同步

本仓库 `docs/` 是 **文档源（Source of Truth）**，支持 **简体中文 + English** 双语，通过 CI 同步到 [GitHub Wiki](https://github.com/kongusen/deepstrike/wiki)。

同时，`main` 分支 push 也会通过 VitePress 部署 **GitHub Pages**（`.github/workflows/deploy-docs.yml`）。

## 双通道

| 通道 | 触发 | 产物 |
|------|------|------|
| GitHub Pages | `docs/**` push → `npm run docs:build` | VitePress 站点（`/en/` 英文路径 + 语言切换） |
| GitHub Wiki | `docs/**` push → `sync-docs-to-wiki.py` | Wiki 页面 + `_Sidebar.md` |

## 双语 Wiki 命名

| 源文件 | Wiki 页面 |
|--------|-----------|
| `docs/index.md` | `Home` |
| `docs/architecture/overview.md` | `Architecture-Overview` |
| `docs/en/index.md` | `En-Home` |
| `docs/en/architecture/overview.md` | `En-Architecture-Overview` |

英文页面统一加 **`En-`** 前缀，避免与中文页面冲突。

`_Sidebar.md` 分两段：中文导航 + 英文导航，顶部互相链接 Home ↔ En-Home。

## 本地同步（需 Wiki 写权限）

```bash
# 可选：克隆 wiki 仓库
git clone https://github.com/kongusen/deepstrike.wiki.git .wiki-sync

# 执行同步（输出到 .wiki-sync/ 或指定目录）
python3 scripts/sync-docs-to-wiki.py

# 检查 diff 后 push
cd .wiki-sync && git add -A && git commit -m "docs: sync from docs/" && git push
```

## CI 自动同步

`.github/workflows/sync-wiki.yml` 在 `docs/**` 变更时 push 到 `deepstrike.wiki` 仓库。

需在仓库 Settings → Features 中启用 Wiki。

## 编辑流程

1. **只改 `docs/`** — 不要直接在 Wiki UI 编辑（会被 CI 覆盖）
2. 中文改 `docs/...`；英文改 `docs/en/...`
3. 本地 `npm run docs:dev` 预览 VitePress（含语言切换）
4. PR merge 到 `main` → 自动 sync Wiki + Pages

## 链接转换

同步脚本会：

- 去掉 VitePress frontmatter
- 将 locale 内链转为 Wiki 页面名（中文 → `Guides-Workflow`，英文 → `En-Guides-Workflow`）
- 忽略跨 locale 链接的自动转换（需手动写 Wiki 页面名或使用绝对 URL）

## VitePress i18n 配置

见 `docs/.vitepress/config.mts`：

- `locales.root` — 简体中文，`docs/` 根目录
- `locales.en` — English，`docs/en/`，URL 前缀 `/en/`

Sidebar / nav 定义在 `docs/.vitepress/shared.ts`，避免重复维护。
