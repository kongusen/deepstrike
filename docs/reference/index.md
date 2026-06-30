# 参考文档

API 与配置的结构化参考。SDK 详细用法见各语言 README。

## 章节

| 文档 | 内容 |
|------|------|
| [RuntimeOptions](./runtime-options) | Python runner 全部配置项 |
| [WorkflowNodeSpec](./workflow-node-spec) | 工作流节点字段 |
| [Python API](./python-api) | `deepstrike` 包导出索引 |

## 源码索引

| 主题 | 路径 |
|------|------|
| Kernel ABI | `crates/deepstrike-core/src/runtime/kernel.rs` |
| Context | `crates/deepstrike-core/src/context/` |
| Python 导出 | `python/deepstrike/__init__.py` |

## 跨语言

Node SDK 与 Python SDK 在 OS 能力上保持 parity，详见仓库 `node/README.md` 与 `python/README.md`。
