# Context Item 表

模型上下文里的每一段内容都是一个 **item**。它来自哪里（origin），决定了它的四个默认属性：能被谁看到（作用域）、以什么形态渲染（形态）、什么时候离开（寿命）、谁可以写它（写入权）。本页只放这张表和每一列在代码里由哪里执行；其它文档引用这里，不再各自解释。

**代码**：`crates/deepstrike-core/src/context/item.rs`（`ItemOrigin::defaults`）、`context/authority.rs`、`context/voice.rs`

## 四列与执行位置

| 列 | 类型 | 在哪里执行 |
|---|---|---|
| 作用域 | `ItemScope` | renewal 只保留 `survives_renewal()` 的 knowledge（`context/renewal.rs`）；子 run 只继承 `inherited_by(inheritance)` 的 knowledge（`ContextManager::inheritable_knowledge`） |
| 形态 | `Fidelity` | 两处记录：history item 记录自己的形态（完整，或压缩改写后的摘录），handle 表记录结果正文在哪里（驻留、外部、换出、被 provider 清理）。渲染器按 handle 表把被清理或换出的结果投影成一行 handle（`context/renderer.rs`）；lifecycle report 与 item 图用 `ContextManager::history_fidelity` 合并两者 |
| 寿命 | `Lease` | knowledge item 的寿命由 `sweep_expired_item_leases` 在每轮执行；skill 的激活租约由 `sweep_expired_skill_leases` 执行，到期时一并移除 skill 正文 |
| 写入权 | `Authority` | 表本身是 `admit_write(origin, writer, op)`（`context/authority.rs`，一个测试固定了全部格子）。宿主替换、删除 knowledge 时经它检查（`ContextManager::check_host_knowledge_write`）；模型的 `update_plan` 经 `admit_task_update` 检查 |

## Origin 默认值

| origin | 作用域 | 形态 | 寿命 | 作者 |
|---|---|---|---|---|
| `system` | operation | full | permanent | kernel |
| `skill` | task | full | 直到 skill 停用 | model |
| `memory_recall` | session | handle | 8 轮未被使用 | host |
| `reference` | operation | full | permanent | host |
| `blackboard` | workflow | full | permanent | host |
| `tool_result` | task | full | permanent | host |
| `dependency_output` | edge | handle | permanent | kernel |
| `user` | operation | full | permanent | host |
| `assistant` | operation | full | permanent | model |
| `provider_summary` | operation | full | permanent | provider |

宿主钉住（pin）的 item 不受轮数限制，但仍会随结束它用途的事件离开（skill 停用时它的正文照样离开）。

## 写入权

| 写入方 | 可以做什么 |
|---|---|
| kernel | 写任何 item |
| host | 创建任何 item；替换、删除除 kernel 和 provider 所写之外的 item |
| model | 只写作者是 model 的 origin（skill、assistant） |
| provider | 只写作者是 provider 的 origin（provider_summary） |

`[TASK STATE]` 的字段：模型经 `update_plan` 只能设 `plan` 与 `current_step`；宿主还能设 `directives`（用户的长期指令）与 `preserved_refs`（免于压缩的结果）。一次更新要么整体接受，要么整体拒绝。

## 不是 item 的东西

signal、`[TASK STATE]` 块和模型的笔记都渲染在 state turn 里，但不存为 item：signal 是 `SignalItem`，在携带它的请求结清时离开；笔记存在 task state 里，内核为它们分配 item id，只用于记录笔记与结果之间的边。

## 内核的话

内核写给模型的所有文本（signal、提示、替调用回答的结果、拒绝理由、`[TASK STATE]` 块、内核工具的说明）都来自 `context/voice.rs`。lifecycle report 的 `kernel_text_tokens` 统计模型看到了多少内核文本。

## 延伸阅读

- [Context 工程](../guides/context-engineering)
- [Prompt Cache 设计](./prompt-cache-design)
- [DEC-13：模型主导上下文](../decisions/013-model-led-context)
