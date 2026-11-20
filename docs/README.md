# DeepStrike 文档

先选一件你想让助手做的事。第一次接触 DeepStrike，可以先看[四张章鱼动图](https://github.com/kongusen/deepstrike/blob/main/README.zh-CN.md#一份周报是怎么写出来的)，或直接[跑一个会查资料的助手](https://github.com/kongusen/deepstrike/blob/main/README.zh-CN.md#运行第一个助手)。

- 我有一个函数，想让模型调用它。看[工具接入](./guides/execution-plane-and-tools.md)。
- 我想让助手查很多资料，还能记得已经确认的结论。看[上下文管理](./guides/context-engineering.md)和[跨会话记忆](./guides/memory.md)。
- 我想把团队的审阅步骤写成助手能按需加载的说明。看 [Skill 的加载、工具和租约](./guides/skills.md)。
- 我想让几个助手分工，读完各自的材料再汇总。看[工作流](./guides/workflow.md)。
- 我想让人批准后才发送，或让中断的任务继续。看[权限与审批](./guides/governance.md)和[会话恢复](./guides/session-replay-and-recovery.md)。

[中文文档首页](./index.md) · [English documentation](./en/index.md) · [实现原理](./architecture/execution-model.md) · [API 参考](./reference/index.md)

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
