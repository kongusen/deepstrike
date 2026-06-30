# DeepStrike 文档

文档源目录，支持 **简体中文（默认）** 与 **English** 双语。

## 目录结构

```
docs/
├── index.md                 # 中文首页（VitePress root locale）
├── en/                      # 英文 locale（/en/ 路径）
│   ├── index.md
│   ├── architecture/
│   ├── getting-started/
│   ├── guides/
│   ├── concepts/
│   └── reference/
├── architecture/            # 中文页面（与 en/ 镜像）
├── ...
└── .vitepress/
    ├── config.mts           # locales: root + en
    └── shared.ts            # 共享 sidebar / nav 定义
```

## 本地预览

```bash
npm ci
npm run docs:dev
```

```bash
npm run docs:dev
# 中文 → /
# 英文 → /en/
```

## 翻译约定

| 规则 | 说明 |
|------|------|
| 文件镜像 | 每个 `docs/foo/bar.md` 对应 `docs/en/foo/bar.md` |
| 内部链接 | 中文页用 `/architecture/overview`；英文页用 `/en/architecture/overview` |
| 代码 | API 名称、代码块保持英文；注释随文档语言 |
| 同步更新 | 改中文时尽量同时改英文；PR 可只改一种语言并标注 TODO |

## 部署

| 通道 | 触发 | 说明 |
|------|------|------|
| GitHub Pages | push `docs/**` | VitePress，`deploy-docs.yml` |
| GitHub Wiki | push `docs/**` | `sync-docs-to-wiki.py`，中文 + `En-*` 页面 |

详见 [wiki/README.md](./wiki/README.md)。

## 添加新页面 checklist

1. 创建 `docs/<section>/<page>.md`（中文）
2. 创建 `docs/en/<section>/<page>.md`（英文）
3. 在 `docs/.vitepress/shared.ts` 的 `sidebar()` 中加入链接
4. 在 `scripts/sync-docs-to-wiki.py` 的 `SIDEBAR_ZH` / `SIDEBAR_EN` 中加入 Wiki 链接（若需出现在 Wiki 侧栏）
