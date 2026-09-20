# 统一公开 Agent 模型实施计划

## 概览

把 Node SDK 的公开心智模型从“用户组装 RuntimeRunner”替换为“用户创建可执行 Agent”。底层 Kernel、Provider adapter、SessionLog 和现有执行循环继续作为内部实现，但根入口只暴露 Agent、Run、Session、工具和面向场景的高级封装。

## 架构决策

- `createAgent(definition)` 是唯一的普通用户创建入口。
- Agent 定义是可序列化的；执行状态不放在定义对象中，而放在 Run 和 AgentSession 中。
- `Agent.run()` 返回结构化 `RunResult<T>`；`collectText()` 只作为流式文本辅助函数。
- Provider 是 Agent 定义的必要依赖，RuntimeRunner 不再是用户必须理解的类型。
- Memory、治理、Signals、委托、Workflow 和验证都通过 Agent/Session 的场景方法进入。
- 旧根入口直接删除，不设计兼容 alias 或双轨文档。

## 任务

### 阶段 1：公开契约

- [x] 任务 1：定义 Agent、Run、Session 的公开类型
  - 验收：`AgentDefinition`、`AgentRunOptions`、`RunResult`、`AgentSession`、`DelegationRequest` 和相关结果类型有明确输入输出。
  - 验证：类型测试覆盖最小 Agent、结构化输出、session 引用和取消信号。
  - 文件：`node/src/agent.ts`、`node/src/session.ts`、`node/src/types.ts`、`node/tests/agent-surface-types.test.ts`

- [x] 任务 2：实现 Agent facade
  - 验收：`createAgent(definition)` 返回对象，支持 `run`、`stream`、`session`；用户不需要直接创建 `RuntimeRunner`。
  - 验证：最小文本调用、流式调用和 session 连续调用通过集成测试。
  - 文件：`node/src/agent.ts`、新增 `node/src/agent-facade.ts`、`node/tests/agent-facade.test.ts`

### 阶段 2：普通用户高级能力

- [x] 任务 3：封装 Memory 和 Session
  - 验收：`agent.remember`、`agent.recall`、`agent.session(id)` 能使用现有 memory store 和 session log，用户不接触 syscall 或事件重放。
  - 验证：记忆写入、查询、跨 run 恢复和失败结果测试。
  - 文件：`node/src/agent-facade.ts`、`node/src/memory/public.ts`、`node/tests/agent-memory.test.ts`

- [ ] 任务 4：封装权限、治理和中断
  - 验收：工具的审批请求可通过 `AgentRunOptions.onPermissionRequest` 处理；Session 能中断当前 Run；错误统一落到 `RunResult.status` 或结构化异常。
  - 验证：允许、拒绝、等待审批和取消测试。
  - 文件：`node/src/agent-facade.ts`、`node/src/runtime/runner.ts`、`node/tests/agent-governance.test.ts`

- [x] 任务 5：封装委托和 Workflow
  - 验收：`agent.delegate` 支持一次专门任务；`agent.workflow` 支持并行节点、依赖和综合结果；用户传入 Agent/目标，而不是手写 kernel task。
  - 验证：单委托、并行 fanout、依赖 join、部分失败和预算拒绝测试。
  - 文件：`node/src/agent-facade.ts`、`node/src/workflow/public.ts`、`node/tests/agent-workflow.test.ts`

- [ ] 任务 6：封装 Signals 和验证
  - 验收：外部事件可以唤醒 Session；验证选项可以返回用户可读的 verdict/evidence；底层 SignalGateway 和 Harness 不出现在普通调用中。
  - 验证：事件唤醒、恢复、验证通过和失败测试。
  - 文件：`node/src/agent-facade.ts`、`node/src/signals/types.ts`、`node/src/harness/public.ts`、`node/tests/agent-signals.test.ts`

### 阶段 3：公开表面替换

- [ ] 任务 7：重建根入口和 subpath
  - 验收：根入口只导出统一 Agent 模型、Provider 工厂、工具、基础结果类型；`RuntimeRunner`、`runAgent`、`runFanout` 不再从根入口导出；高级扩展集中到 `@deepstrike/sdk/advanced` 或同等 subpath。
  - 验证：API surface 测试和 TypeScript 构建通过。
  - 文件：`node/src/index.ts`、`node/package.json`、新增 `node/src/advanced/public.ts`、`node/tests/api-surface.test.ts`

- [ ] 任务 8：重写文档和示例
  - 验收：Quick start、getting started 和 API reference 只展示 `createAgent`、`agent.run`、`agent.stream`；高级能力按用户任务组织。
  - 验证：文档 drift 检查、README 示例类型检查和 docs build 通过。
  - 文件：`node/README.md`、`README.md`、`README.zh-CN.md`、`docs/getting-started/*`、`docs/reference/*`

## 检查点

### 检查点 1：契约

- 类型可以表达 Agent、Run、Session、Runtime、Workflow 的边界。
- 最小 Agent API 无需导入 RuntimeRunner。
- 没有把 Kernel 类型泄漏到 Tier 1。

### 检查点 2：能力封装

- Memory、审批、中断、委托、Workflow 和恢复都可以通过 Agent/Session 方法完成。
- 每个 facade 都有用户语义的结果和错误。
- 底层事件仍然可以由高级用户通过 advanced subpath 访问。

### 检查点 3：表面替换

- 根入口没有旧入口。
- 新 README 的最小示例可以构建和运行。
- Node 测试、类型检查和文档检查通过。

## 风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| Agent facade 绑定 RuntimeRunner 内部状态 | Agent 无法复用或并发执行 | 定义对象保持不可变，Run/Session 持有执行状态，每次调用创建独立上下文 |
| 高级方法数量继续膨胀 | Agent 重新变成大杂烩 | 只保留用户任务入口；Kernel、journal、replay、evolution 进入 advanced subpath |
| `RunResult` 改变错误语义 | 用户无法区分失败和部分完成 | 统一 status、error code、output 和 evidence 字段，并用契约测试固定 |
| Provider 作为 Agent 必填项限制部署 | 工厂和路由无法延后决定 | Provider 先作为显式依赖；后续可在 `createAgent` 外增加应用级 provider registry |
| 一次性删除旧 API 导致内部引用残留 | 构建和文档大范围失败 | 先完成 facade 和测试，再由 API surface 检查驱动全仓迁移，最后删除旧导出 |

## 验证命令

```bash
cd node
npm run build
npm test -- --runInBand
cd ..
node scripts/check-docs-drift.mjs
npm run docs:build
```
