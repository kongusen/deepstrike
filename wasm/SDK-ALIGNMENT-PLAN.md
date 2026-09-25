# WASM SDK 体系化对齐计划

## 目标

在不复制 Kernel 逻辑的前提下，把 WASM SDK 的公共 Agent API、运行生命周期和浏览器 transport 对齐到 Node/Python 已经稳定的共享语义。WASM 采用浏览器友好的 TypeScript API，所有调度、审计、恢复和工作流状态继续落到现有 Runtime/Canonical Kernel。

## 分层边界

1. **Kernel 层**：沿用 `crates/deepstrike-wasm` 与现有 `CanonicalRunnerRuntime`，不新增第二套状态机。
2. **Runtime 层**：复用 `RuntimeRunner`、`SessionLog`、memory、workflow、journal、execution evidence。
3. **Agent facade 层**：补齐 declaration、session、run result、resume、memory、handoff、signals、interrupt、close。
4. **Transport 层**：定义浏览器可用的 MCP HTTP/SSE/custom connection contract，stdio 只在 Node 环境提供。
5. **Verification 层**：每个能力补 contract test，并运行 TypeScript build、Jest、WASM canonical binding tests。

## 实施阶段

### Phase 1: Agent contract foundation

- [ ] Stable `AgentSession` bound to one `SessionLog`
- [ ] Structured `AgentRunResult` with status, usage, run id and evidence
- [ ] Per-run options: metadata, provider options, timeout, token cap, attachments, cancellation
- [ ] Resume, interrupt and close lifecycle

### Phase 2: Kernel-backed capabilities

- [ ] Durable memory `remember/recall`
- [ ] Declared handoff/delegate
- [ ] Signal `listen` with claim/ack/nack
- [ ] Workflow execution, trace and replay projection

### Phase 3: Browser transport and persistence

- [ ] MCP connection protocol and HTTP/SSE/custom adapter seam
- [ ] Automatic Agent transport lifecycle
- [ ] Durable session/payload storage adapter contracts

### Phase 4: Release verification

- [ ] TypeScript build passes
- [ ] WASM Jest suite passes
- [ ] Canonical binding/golden fixture sweep passes
- [ ] Public API export matrix documented

## Acceptance criteria

- WASM facade uses the same `AgentRunSpec`, session event vocabulary and Kernel lifecycle as Node/Python.
- No provider, memory, workflow, or MCP implementation introduces a second state machine.
- Browser builds do not depend on Node-only APIs such as subprocesses or filesystem access.
- Existing WASM tests remain green after every phase.
