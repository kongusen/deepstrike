# DeepStrike 文档

文档源目录，支持 **简体中文（默认）** 与 **English** 双语。默认阅读路径从要构建的 Agent 出发，而不是从运行时实现出发。

## 阅读路径

1. **入门**：安装 SDK，运行第一个使用工具的 Agent，并选择合适的 API。
2. **Agent 能力**：按需添加模型、工具、Memory、Skill、治理、委托、工作流、Signal 和可恢复 Session。
3. **教程课程**：在 `example/` 的 Research Brief Studio 中，从单 Agent 逐级构建到协作 Agent 团队。
4. **Agent 如何运行**：理解一次运行、Context、时间、质量和连续性如何配合。
5. **概念**：阅读角色、隔离、缓存和预算等设计概念。
6. **参考**：查找 API、字段、选项和事件类型。

`architecture/overview` 与 `architecture/kernel-abi` 是实现细节，保留给需要研究运行时实现或 ABI 的读者，不作为新用户的起点。

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

可运行教程位于仓库根目录的 `example/`，不复制到 `docs/`。文档中通过教程课程导航和首页链接把读者带到对应等级。

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
