#!/usr/bin/env node

/**
 * Generate the DeepStrike architecture diagram suite.
 *
 * The diagrams intentionally share the restrained visual language introduced by
 * readme_agent_os_map.svg: graphite surfaces, ivory type, one coral accent, thin
 * rules, compact labels, and explicit ownership boundaries.
 */

import { mkdir, writeFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import path from "node:path"

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")
const OUT = path.join(ROOT, "docs", "public")
const VERSION = "0.2.48"
const W = 1200
const H = 760

const escapeXml = value => String(value)
  .replaceAll("&", "&amp;")
  .replaceAll("<", "&lt;")
  .replaceAll(">", "&gt;")

const text = (x, y, value, cls = "body", anchor = "start") =>
  `<text x="${x}" y="${y}" class="${cls}" text-anchor="${anchor}">${escapeXml(value)}</text>`

const lines = (x, y, values, cls = "body", gap = 19, anchor = "start") =>
  values.map((value, i) => text(x, y + i * gap, value, cls, anchor)).join("\n")

const rect = (x, y, w, h, cls = "panel", rx = 6) =>
  `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="${rx}" class="${cls}"/>`

const rule = (x1, y1, x2, y2, cls = "rule") =>
  `<path d="M${x1} ${y1}H${x2}" class="${cls}"/>`

const arrow = (d, coral = false, dashed = false) =>
  `<path d="${d}" class="flow${coral ? " flow-coral" : ""}${dashed ? " flow-dash" : ""}"/>`

const pill = (x, y, w, label, tone = "coral") => `
  <rect x="${x}" y="${y}" width="${w}" height="22" rx="11" class="pill ${tone}"/>
  ${text(x + w / 2, y + 15, label, "pill-text", "middle")}`

const card = ({ x, y, w, h, title, body = [], tag, strong = false, accent = false }) => `
  <g>
    ${rect(x, y, w, h, strong ? "panel-strong" : "panel")}
    ${accent ? `<rect x="${x}" y="${y}" width="5" height="${h}" rx="2.5" class="accent-fill"/>` : ""}
    ${text(x + 18, y + 29, title, "card-title")}
    ${body.length ? lines(x + 18, y + 53, body, "card-text", 18) : ""}
    ${tag ? text(x + w - 16, y + 28, tag, "micro", "end") : ""}
  </g>`

const section = (y, title, note = "") => `
  ${text(48, y, title, "section-title")}
  ${note ? text(1152, y, note, "section-note", "end") : ""}`

const shell = ({ title, desc, eyebrow, headline, subtitle, body, footer = "DEEPSTRIKE · CONTROL FLOW IN THE KERNEL · REAL I/O IN THE HOST" }) => `
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" role="img" aria-labelledby="title desc">
  <title id="title">${escapeXml(title)}</title>
  <desc id="desc">${escapeXml(desc)}</desc>
  <defs>
    <marker id="arrow-ivory" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto">
      <path d="M0 1 10 5 0 9Z" fill="#F7F3EA"/>
    </marker>
    <marker id="arrow-coral" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto">
      <path d="M0 1 10 5 0 9Z" fill="#FF6B4A"/>
    </marker>
    <style>
      text { font-family: Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", "Noto Sans CJK SC", sans-serif; }
      .eyebrow { fill:#FF6B4A; font-size:11px; font-weight:700; letter-spacing:2px; }
      .title { fill:#F7F3EA; font-size:29px; font-weight:700; letter-spacing:-.45px; }
      .subtitle { fill:#A8A49C; font-size:13px; font-weight:450; }
      .section-title { fill:#F7F3EA; font-size:15px; font-weight:700; }
      .section-note { fill:#8F8C86; font-size:10px; font-weight:600; letter-spacing:.65px; }
      .card-title { fill:#F7F3EA; font-size:13px; font-weight:650; }
      .card-text { fill:#A8A49C; font-size:10.5px; font-weight:450; }
      .body { fill:#A8A49C; font-size:11px; font-weight:450; }
      .label { fill:#F7F3EA; font-size:11px; font-weight:650; }
      .micro { fill:#8F8C86; font-size:9.5px; font-weight:650; letter-spacing:.55px; }
      .pill-text { fill:#111318; font-size:9px; font-weight:750; letter-spacing:.5px; }
      .footer { fill:#77746F; font-size:9px; font-weight:600; letter-spacing:.8px; }
      .panel { fill:#15171A; stroke:#303236; stroke-width:1; }
      .panel-strong { fill:#191A1D; stroke:#F7F3EA; stroke-width:1.15; }
      .kernel { fill:#0D0F11; stroke:#3B3D41; stroke-width:1.2; }
      .soft { fill:#121316; stroke:#26282C; stroke-width:1; }
      .accent-fill { fill:#FF6B4A; }
      .pill.coral { fill:#FF6B4A; }
      .pill.ivory { fill:#F7F3EA; }
      .pill.muted { fill:#8F8C86; }
      .rule { fill:none; stroke:#303236; stroke-width:1; }
      .rule-coral { fill:none; stroke:#FF6B4A; stroke-width:1.2; }
      .flow { fill:none; stroke:#F7F3EA; stroke-width:1.45; marker-end:url(#arrow-ivory); }
      .flow-coral { stroke:#FF6B4A; stroke-width:1.75; marker-end:url(#arrow-coral); }
      .flow-dash { stroke-dasharray:5 5; }
    </style>
  </defs>
  <rect width="${W}" height="${H}" fill="#111318"/>
  <path d="M0 94H${W}M0 724H${W}" class="rule"/>
  ${text(48, 33, `${eyebrow} / v${VERSION}`, "eyebrow")}
  ${text(48, 68, headline, "title")}
  ${text(1152, 39, subtitle, "subtitle", "end")}
  <rect x="1135" y="70" width="17" height="3" rx="1.5" class="accent-fill"/>
  ${body}
  ${text(48, 746, footer, "footer")}
</svg>
`

const diagrams = new Map()
const add = (name, config) => diagrams.set(name, shell(config))

function addRuntimeMap({ zh = false } = {}) {
  const t = zh ? {
    file: "readme_agent_os_map_zh.svg",
    title: "DeepStrike 运行机制",
    desc: "DeepStrike 0.2.48 的完整运行边界：宿主拥有真实 I/O，RuntimeRunner 驱动 ABI v2，Rust 内核治理控制流，Self-Harness v2 在隔离外环中改进下一次运行。",
    eyebrow: "运行机制",
    headline: "所有 Agent 副作用，共用一个受治理的控制环。",
    subtitle: "内核负责控制流 · 宿主负责真实 I/O",
    host: "宿主拥有的 I/O",
    hostNote: "凭据 · 网络 · 文件系统 · 持久存储",
    runtime: "宿主运行循环",
    kernel: "deepstrike-core",
    harness: "Self-Harness v2 外环",
    sdk: "同一 ABI 的宿主 SDK",
  } : {
    file: "readme_agent_os_map.svg",
    title: "DeepStrike runtime mechanism",
    desc: "The complete DeepStrike 0.2.48 runtime boundary: host-owned I/O, RuntimeRunner driving ABI v2, a Rust kernel governing control flow, and an isolated Self-Harness v2 outer loop improving the next run.",
    eyebrow: "RUNTIME MECHANISM",
    headline: "One governed loop for every agent effect.",
    subtitle: "CONTROL FLOW IN THE KERNEL · REAL I/O IN THE HOST",
    host: "Host-owned I/O",
    hostNote: "CREDENTIALS · NETWORK · FILESYSTEM · DURABLE STORAGE",
    runtime: "Host runtime loop",
    kernel: "deepstrike-core",
    harness: "Self-Harness v2 outer loop",
    sdk: "Host SDKs on one ABI",
  }

  const hostCards = zh ? [
    ["Provider 适配器", "流式 LLM · 模型路由", "文本 · 图像 · 音频序列化"],
    ["ExecutionPlane", "工具 · 流式 · 挂起 / 恢复", "Worktree · Sandbox · Remote VPC"],
    ["证据与持久存储", "SessionLog · ABI 事务 · 快照", "DreamStore · Archive · 大结果 spool"],
    ["外部控制", "审批 · 信号 · Webhook · Cron", "取消 · Deadline · 宿主策略"],
  ] : [
    ["Provider adapters", "Streaming LLM calls · model routing", "Text · image · audio serialization"],
    ["ExecutionPlane", "Tools · streaming · suspend / resume", "Worktrees · sandboxes · remote VPC"],
    ["Evidence & durable stores", "SessionLog · ABI transactions · snapshots", "DreamStore · archives · large-result spool"],
    ["External control", "Approvals · signals · webhooks · cron", "Cancellation · deadlines · host policy"],
  ]
  const kernelCards = zh ? [
    ["Syscall Gate 与治理", "Invoke · Spawn · Memory", "SubmitNodes · LoadWorkflow", "允许 · 拒绝 · 询问 · 限流"],
    ["调度器、进程树与 DAG", "TCB · 子 Agent · 依赖屏障", "预算 · 配额 · 信任 · 隔离", "LOOP · CLASSIFY · REDUCE"],
    ["Context VM 与知识", "稳定 · 知识 · 历史 · 状态", "压缩 · Handle · Cache 边界", "SKILL LEASE · KNOWLEDGE BUDGET"],
    ["Observation 与恢复", "Provider / Tool 结果 · 可见拒绝", "信号 · 事务 · 快照 · 唤醒", "APPEND-ONLY EVIDENCE"],
  ] : [
    ["Syscall gate & governance", "Invoke · Spawn · memory", "SubmitNodes · LoadWorkflow", "ALLOW · DENY · ASK · RATE LIMIT"],
    ["Scheduler, process tree & DAG", "TCBs · child agents · dependency barriers", "Budgets · quota · trust · isolation", "LOOP · CLASSIFY · REDUCE"],
    ["Context VM & knowledge", "stable · knowledge · history · state", "Compression · handles · cache boundaries", "SKILL LEASE · KNOWLEDGE BUDGET"],
    ["Observation & recovery", "Provider / tool results · visible denials", "Signals · transactions · snapshots · wake", "APPEND-ONLY EVIDENCE"],
  ]

  const body = `
    ${section(122, t.host, t.hostNote)}
    ${hostCards.map((c, i) => card({ x: 48 + i * 276, y: 140, w: i === 3 ? 276 : 258, h: 82, title: c[0], body: c.slice(1), accent: i === 0 })).join("\n")}
    ${section(260, t.runtime, "RUNTIMERUNNER · ACTIONS OUT · OBSERVATIONS IN")}
    ${card({ x: 48, y: 278, w: 1104, h: 48, title: "RuntimeRunner", body: [], strong: true, accent: true })}
    ${text(210, 308, zh ? "分派 KernelEffect" : "dispatch KernelEffect", "card-text")}
    ${text(408, 308, zh ? "执行获批 Effect" : "execute approved effects", "card-text")}
    ${text(620, 308, zh ? "追加证据" : "append evidence", "card-text")}
    ${text(760, 308, zh ? "回灌 Observation" : "feed observations", "card-text")}
    ${text(928, 308, zh ? "信号 · Harness 指令" : "signals · harness instructions", "card-text")}
    ${arrow("M488 326V347H386V368")}
    ${arrow("M704 368V347H592V326", true)}
    ${text(410, 350, zh ? "EFFECT 向外" : "EFFECTS OUT", "micro")}
    ${text(720, 350, zh ? "OBSERVATION 向内" : "OBSERVATIONS IN", "micro")}
    ${rect(48, 370, 1104, 214, "kernel", 7)}
    ${text(68, 400, t.kernel, "section-title")}
    ${text(205, 400, zh ? "纯 Rust · 可序列化状态机 · ABI v2 · 零 Provider I/O" : "PURE RUST · SERIALIZABLE STATE MACHINE · ABI v2 · ZERO PROVIDER I/O", "section-note")}
    ${pill(1067, 384, 66, zh ? "内核" : "KERNEL")}
    ${kernelCards.map((c, i) => {
      const x = 68 + i * 261
      return `${card({ x, y: 422, w: i === 3 ? 281 : 249, h: 136, title: c[0], body: c.slice(1, 3) })}
        ${rule(x + 18, 509, x + (i === 3 ? 263 : 231))}
        ${text(x + 18, 536, c[3], "micro")}`
    }).join("\n")}
    ${section(620, t.harness, zh ? "只改下一次运行 · 不绕过治理" : "IMPROVES THE NEXT RUN · NEVER BYPASSES GOVERNANCE")}
    ${card({ x: 48, y: 638, w: 770, h: 62, title: zh ? "证据 → 聚类 → 提案 → 筛查 → held-in / held-out 验证 → 分级晋升" : "evidence → cluster → propose → screen → held-in / held-out validation → tiered promote", body: [zh ? "Scope 隔离 · 能力面只取交集 · 内容寻址谱系" : "scope isolation · capability intersection · content-addressed lineage"], accent: true })}
    ${card({ x: 838, y: 638, w: 314, h: 62, title: t.sdk, body: ["Node.js · Python · Rust · WASM"] })}
    ${arrow("M818 669H836", true, true)}
  `

  add(t.file, {
    title: t.title,
    desc: t.desc,
    eyebrow: t.eyebrow,
    headline: t.headline,
    subtitle: t.subtitle,
    body,
    footer: zh ? "DEEPSTRIKE · 控制流在内核 · 真实 I/O 在宿主" : undefined,
  })
}

addRuntimeMap()
addRuntimeMap({ zh: true })

add("agent_os_architecture.svg", {
  title: "DeepStrike Agent OS architecture",
  desc: "Layered system architecture showing application APIs, the host runtime and I/O adapters, the ABI v2 boundary, the pure Rust kernel, and durable evidence stores.",
  eyebrow: "SYSTEM ARCHITECTURE",
  headline: "A narrow kernel boundary keeps authority explicit.",
  subtitle: "FIVE LAYERS · ONE EVIDENCE CHAIN",
  body: `
    ${section(124, "Application intent", "WHAT BUILDERS AUTHOR")}
    ${card({ x: 48, y: 142, w: 258, h: 72, title: "runAgent / RuntimeRunner", body: ["Goals · tools · attachments · signals"] })}
    ${card({ x: 324, y: 142, w: 258, h: 72, title: "WorkflowSpec", body: ["DAG · control nodes · output schemas"] })}
    ${card({ x: 600, y: 142, w: 258, h: 72, title: "Policies & profiles", body: ["Governance · quotas · reliability"] })}
    ${card({ x: 876, y: 142, w: 276, h: 72, title: "HarnessManifest", body: ["Bounded next-run adaptation"] })}

    ${section(254, "Host user space", "NODE.JS · PYTHON · RUST · WASM")}
    ${card({ x: 48, y: 272, w: 210, h: 100, title: "RuntimeRunner", body: ["Drive effects", "Append observations"], accent: true })}
    ${card({ x: 274, y: 272, w: 210, h: 100, title: "Provider adapters", body: ["Vendor wire formats", "Streaming + replay"] })}
    ${card({ x: 500, y: 272, w: 210, h: 100, title: "ExecutionPlane", body: ["Local · worktree", "sandbox · remote"] })}
    ${card({ x: 726, y: 272, w: 210, h: 100, title: "Stores", body: ["SessionLog · DreamStore", "archive · spool"] })}
    ${card({ x: 952, y: 272, w: 200, h: 100, title: "External control", body: ["Approvals · signals", "cancel · deadlines"] })}

    ${arrow("M600 372V408", true)}
    ${text(618, 398, "KernelEffect ↓     ↑ KernelObservation", "micro")}
    ${rect(48, 414, 1104, 210, "kernel", 7)}
    ${text(68, 444, "deepstrike-core", "section-title")}
    ${text(207, 444, "PURE STATE MACHINE · ABI v2 · NO NETWORK / FILESYSTEM / CREDENTIALS", "section-note")}
    ${pill(1064, 428, 69, "KERNEL")}
    ${card({ x: 68, y: 466, w: 250, h: 130, title: "Syscall & governance", body: ["Effects enter one trap", "Policy + quota + constraints"], tag: "P1" })}
    ${card({ x: 330, y: 466, w: 250, h: 130, title: "TCB scheduler & DAG", body: ["Lifecycle · joins · budgets", "Loop · classify · tournament"], tag: "P2" })}
    ${card({ x: 592, y: 466, w: 250, h: 130, title: "Context VM", body: ["Four slots · compression", "handles · skills · knowledge"], tag: "P3" })}
    ${card({ x: 854, y: 466, w: 278, h: 130, title: "Transactions & recovery", body: ["Accepted input journal", "snapshots · replay · repair"] })}

    ${section(666, "Durable evidence plane", "AUDIT · REPLAY · RECOVERY · EVAL")}
    ${text(48, 694, "SessionLog records host events; KernelSnapshot rebuilds accepted ABI state; OS Snapshot folds observations for dashboards.", "body")}
  `,
})

add("agent_os_loop_flow.svg", {
  title: "DeepStrike L-star loop and syscall trap",
  desc: "One turn through Reason, Act, Adjudicate, Execute, Observe, and Delta, including visible denial results and suspension for approval.",
  eyebrow: "EXECUTION MODEL",
  headline: "A turn is a state transition, not an SDK callback chain.",
  subtitle: "REASON → ACT → ADJUDICATE → OBSERVE → DELTA",
  body: `
    ${section(124, "One task turn", "TASK LIFECYCLE IS ORTHOGONAL: READY · RUNNING · SUSPENDED · DONE")}
    ${card({ x: 48, y: 148, w: 208, h: 120, title: "1 · Reason", body: ["Render four context slots", "Narrow visible tool schemas", "Emit CallProvider"], accent: true })}
    ${card({ x: 280, y: 148, w: 208, h: 120, title: "2 · Act", body: ["Provider returns text", "and typed tool calls", "Trap every requested effect"] })}
    ${card({ x: 512, y: 148, w: 208, h: 120, title: "3 · Adjudicate", body: ["Veto · rate · permission", "constraints · resource quota", "Allow / Deny / Gate / Defer"] })}
    ${card({ x: 744, y: 148, w: 208, h: 120, title: "4 · Execute", body: ["Host runs approved calls", "Meta-tools stay in kernel", "No denied effect executes"] })}
    ${card({ x: 976, y: 148, w: 176, h: 120, title: "5 · Observe", body: ["Results return", "Evidence appends", "DAG may advance"] })}
    ${arrow("M256 208H278")}${arrow("M488 208H510")}${arrow("M720 208H742")}${arrow("M952 208H974")}

    ${section(318, "Disposition branches", "DENIALS ARE VISIBLE RESULTS SINCE v0.2.42 · NO TURN ROLLBACK")}
    ${card({ x: 48, y: 338, w: 258, h: 110, title: "Allow", body: ["ExecutionPlane receives the call", "ToolResult returns to history"] })}
    ${card({ x: 324, y: 338, w: 258, h: 110, title: "Deny / RateLimited", body: ["Effect never runs", "Error ToolResult remains visible"], accent: true })}
    ${card({ x: 600, y: 338, w: 258, h: 110, title: "Gate", body: ["Task → Suspended(Approval)", "Host resolves correlated request"] })}
    ${card({ x: 876, y: 338, w: 276, h: 110, title: "Spawn / Workflow", body: ["Child TCB or DAG nodes", "Parent waits without token burn"] })}

    ${section(500, "Delta boundary", "PRESSURE · COMPACTION · RENEWAL · BUDGET · ENTROPY")}
    ${rect(48, 520, 1104, 116, "panel-strong", 7)}
    ${text(68, 549, "6 · Delta", "card-title")}
    ${text(68, 575, "Sample token pressure and entropy", "card-text")}
    ${arrow("M286 576H404", true)}
    ${text(424, 575, "Snip → Drop → Summarize", "label")}
    ${arrow("M611 576H730", true)}
    ${text(750, 575, "Renew memory / skills / signals", "label")}
    ${arrow("M984 576H1084", true)}
    ${text(68, 611, "If unfinished and within budget, the kernel renders the next turn. Completion, quota, cancellation, or no-progress terminates the TCB.", "body")}
    ${arrow("M1110 636V678H150V270", true, true)}
    ${text(602, 684, "NEXT TURN", "micro", "middle")}
  `,
})

add("agent_os_workflow_dag.svg", {
  title: "DeepStrike workflow DAG execution",
  desc: "A concrete typed workflow DAG showing parallel isolated research nodes, a deterministic reducer, a writer, and a verifier gate, all scheduled through child TCBs.",
  eyebrow: "WORKFLOW DATAFLOW",
  headline: "DAG edges carry outputs, not just ordering.",
  subtitle: "PARALLEL RESEARCH → ZERO-TOKEN REDUCE → WRITE → VERIFY",
  body: `
    ${section(124, "Typed five-node pipeline", "EVERY SPAWN CROSSES THE SAME SYSCALL GATE")}
    ${card({ x: 48, y: 158, w: 224, h: 112, title: "wf-node0 · explore", body: ["Research source A", "outputSchema: Finding[]", "isolation: read_only"], accent: true })}
    ${card({ x: 48, y: 304, w: 224, h: 112, title: "wf-node1 · explore", body: ["Research source B", "outputSchema: Finding[]", "isolation: read_only"], accent: true })}
    ${card({ x: 374, y: 230, w: 224, h: 124, title: "wf-node2 · reduce", body: ["dependsOn: [0, 1]", "reducer: concat", "provider calls: 0"] })}
    ${card({ x: 700, y: 230, w: 224, h: 124, title: "wf-node3 · implement", body: ["Receives reducer output", "Writes structured brief", "modelHint: strong"] })}
    ${card({ x: 1028, y: 230, w: 124, h: 124, title: "wf-node4", body: ["Verifier", "pass / fail", "blocks Done"] })}
    ${arrow("M272 214H322V270H372")}${arrow("M272 360H322V314H372")}${arrow("M598 292H698", true)}${arrow("M924 292H1026", true)}
    ${text(322, 252, "dependsOn", "micro", "middle")}
    ${text(648, 281, "typed output", "micro", "middle")}
    ${text(976, 281, "draft", "micro", "middle")}

    ${section(474, "Kernel scheduling view", "BLOCKED NODES CONSUME NO LLM TOKENS")}
    ${rect(48, 494, 1104, 142, "kernel", 7)}
    ${card({ x: 68, y: 516, w: 238, h: 94, title: "TaskTable", body: ["One child TCB per spawn", "Ready · Running · Suspended · Done"] })}
    ${card({ x: 322, y: 516, w: 238, h: 94, title: "Dependency barrier", body: ["Advance only on durable output", "Schema failure starves dependents"] })}
    ${card({ x: 576, y: 516, w: 238, h: 94, title: "Trust & capability", body: ["Inherited or filtered tools", "Quarantine cannot escalate"] })}
    ${card({ x: 830, y: 516, w: 302, h: 94, title: "Budget & lineage", body: ["Per-node caps + RunGroup settlement", "modelHint resolved only by host"] })}
    ${section(680, "Recovery", "COMPLETED OUTPUTS AND RUNTIME APPENDS REBUILD FROM SESSIONLOG")}
    ${text(48, 708, "resumeWorkflow skips completed nodes, restores submitted nodes, and schedules only the remaining ready frontier.", "body")}
  `,
})

add("workflow_mechanisms.svg", {
  title: "DeepStrike dynamic workflow mechanisms",
  desc: "The workflow vocabulary: static and runtime DAG growth, control-flow nodes, deterministic reducers, scheduler barriers, isolation, quotas, and recovery.",
  eyebrow: "DYNAMIC WORKFLOWS",
  headline: "Orchestration is durable data the kernel can govern.",
  subtitle: "DAG GROWTH · CONTROL FLOW · SCHEDULING · RECOVERY",
  body: `
    ${section(124, "1 · Author and grow", "WORKFLOWS CAN START IN THE HOST OR BE AUTHORED BY THE AGENT")}
    ${card({ x: 48, y: 144, w: 258, h: 102, title: "WorkflowSpec", body: ["Static nodes + dependsOn edges", "Standalone or inside an active run"], accent: true })}
    ${card({ x: 324, y: 144, w: 258, h: 102, title: "submit_workflow_nodes", body: ["Append to active DAG", "Syscall::SubmitNodes"] })}
    ${card({ x: 600, y: 144, w: 258, h: 102, title: "start_workflow", body: ["Bootstrap or flatten into parent", "Syscall::LoadWorkflow"] })}
    ${card({ x: 876, y: 144, w: 276, h: 102, title: "Growth ceiling", body: ["maxWorkflowNodes + quota", "Rejected control request is durable"] })}
    ${arrow("M306 195H322")}${arrow("M582 195H598")}${arrow("M858 195H874", true)}

    ${section(292, "2 · First-class node vocabulary", "PROMPT CONVENTIONS BECOME SCHEDULABLE OBJECTS")}
    ${card({ x: 48, y: 312, w: 202, h: 108, title: "Spawn", body: ["One isolated child TCB", "Role · tools · modelHint"] })}
    ${card({ x: 266, y: 312, w: 202, h: 108, title: "Loop", body: ["loopContinue + maxIters", "May append downstream work"] })}
    ${card({ x: 484, y: 312, w: 202, h: 108, title: "Classify", body: ["Select one branch", "Prune inactive subgraphs"] })}
    ${card({ x: 702, y: 312, w: 202, h: 108, title: "Tournament", body: ["Parallel entrants", "Pairwise judge bracket"] })}
    ${card({ x: 920, y: 312, w: 232, h: 108, title: "Reduce", body: ["Named pure function", "Deterministic · zero LLM tokens"] })}

    ${section(468, "3 · Schedule and recover", "DEPENDENCIES CARRY DATA · BLOCKED NODES DO NOT BURN TOKENS")}
    ${rect(48, 488, 1104, 146, "kernel", 7)}
    ${card({ x: 68, y: 510, w: 250, h: 98, title: "Ready frontier", body: ["TaskGraph computes runnable nodes", "Scheduler policy chooses order"] })}
    ${card({ x: 330, y: 510, w: 250, h: 98, title: "Spawn gate", body: ["Depth · concurrency · total count", "Trust + capability subset"] })}
    ${card({ x: 592, y: 510, w: 250, h: 98, title: "Output boundary", body: ["Schema validation + retry", "Dependents receive durable output"] })}
    ${card({ x: 854, y: 510, w: 278, h: 98, title: "Session recovery", body: ["Fold completions + submissions", "Resume remaining frontier only"] })}
    ${section(680, "Templates", "FAN-OUT / SYNTHESIZE · VERIFY RULES · GENERATE / FILTER · TOURNAMENT")}
  `,
})

add("governance_pipeline.svg", {
  title: "DeepStrike syscall governance funnel",
  desc: "All effect requests cross pre-exposure filtering, veto, rate, permission, parameter constraints, resource quota, and optional host hooks before execution; denials return visible results without rollback.",
  eyebrow: "GOVERNANCE FUNNEL",
  headline: "Authority is enforced below the model.",
  subtitle: "SCHEMA FILTER → SYSCALL TRAP → DISPOSITION → HOST EXECUTION",
  body: `
    ${section(124, "Before the provider call", "STATIC DENIALS CAN DISAPPEAR FROM THE TOOL SCHEMA")}
    ${card({ x: 48, y: 144, w: 334, h: 92, title: "Registered capabilities", body: ["ExecutionPlane tools · meta-tools", "Skill / manifest capability ceiling"] })}
    ${arrow("M382 190H430", true)}
    ${card({ x: 432, y: 144, w: 334, h: 92, title: "Schema pre-filter", body: ["Known-denied tools never reach the model", "Visible set = intersection of all gates"], accent: true })}
    ${arrow("M766 190H814", true)}
    ${card({ x: 816, y: 144, w: 336, h: 92, title: "Provider sees narrowed tools", body: ["No prompt can call a schema-hidden tool", "Dynamic checks still run at call time"] })}

    ${section(282, "At the syscall trap", "INVOKE · SPAWN · WRITE MEMORY · SUBMIT NODES · LOAD WORKFLOW")}
    ${card({ x: 48, y: 302, w: 188, h: 108, title: "1 · Veto", body: ["Hard block list", "Immediate deny"] })}
    ${card({ x: 250, y: 302, w: 188, h: 108, title: "2 · Rate", body: ["Sliding window", "Retry-after result"] })}
    ${card({ x: 452, y: 302, w: 188, h: 108, title: "3 · Permission", body: ["Allow · deny", "ask_user"] })}
    ${card({ x: 654, y: 302, w: 188, h: 108, title: "4 · Constraint", body: ["Required · enum", "numeric range"] })}
    ${card({ x: 856, y: 302, w: 296, h: 108, title: "5 · Resource quota + host hook", body: ["Spawn depth / count · memory rate", "Stateful onToolCall fails closed"] })}
    ${arrow("M236 356H248")}${arrow("M438 356H450")}${arrow("M640 356H652")}${arrow("M842 356H854")}

    ${section(458, "Disposition", "THE MODEL NEVER EXECUTES AN EFFECT DIRECTLY")}
    ${card({ x: 48, y: 478, w: 250, h: 116, title: "Allow", body: ["Host ExecutionPlane may run", "Result returns as observation"] })}
    ${card({ x: 314, y: 478, w: 250, h: 116, title: "Deny / RateLimited", body: ["Effect does not run", "Error ToolResult stays visible"], accent: true })}
    ${card({ x: 580, y: 478, w: 250, h: 116, title: "Gate", body: ["Task suspends for approval", "Correlated resolution resumes"] })}
    ${card({ x: 846, y: 478, w: 306, h: 116, title: "Defer / control rejection", body: ["Backpressure or typed rejection", "Decision is appended durably"] })}
    ${section(642, "Current denial contract", "SINCE v0.2.42: NO ROLLBACK · ALLOWED SIBLINGS STILL EXECUTE")}
    ${text(48, 674, "A dynamic denial closes the tool-call pair with a visible error result, so the next turn can replan from evidence instead of replaying the failed turn.", "body")}
  `,
})

add("reducers_mechanisms.svg", {
  title: "DeepStrike structured output and reducers",
  desc: "Schema-carrying workflow nodes are instructed and validated by the host, retried within a bound, then passed to dependents or reduced by deterministic zero-token functions.",
  eyebrow: "STRUCTURED DATA",
  headline: "Validated outputs make DAG edges reliable inputs.",
  subtitle: "SCHEMA CONTRACT · BOUNDED RETRY · DETERMINISTIC REDUCE",
  body: `
    ${section(124, "LLM node output path", "KERNEL CARRIES THE CONTRACT · SDK VALIDATES THE VALUE")}
    ${card({ x: 48, y: 150, w: 208, h: 112, title: "Workflow node", body: ["task + outputSchema", "Schema travels with descriptor"], accent: true })}
    ${card({ x: 288, y: 150, w: 208, h: 112, title: "Provider prompt", body: ["SDK injects format instruction", "Model returns text"] })}
    ${card({ x: 528, y: 150, w: 208, h: 112, title: "Extract + validate", body: ["Object / array / scalar", "required · properties · items · enum"] })}
    ${card({ x: 768, y: 150, w: 208, h: 112, title: "Bounded retry", body: ["Validation feedback", "1..16 attempts"] })}
    ${card({ x: 1008, y: 150, w: 144, h: 112, title: "Commit", body: ["Typed output", "unblocks DAG"] })}
    ${arrow("M256 206H286")}${arrow("M496 206H526")}${arrow("M736 206H766")}${arrow("M976 206H1006", true)}
    ${arrow("M872 262V290H632V264", true, true)}
    ${text(750, 286, "retry on mismatch", "micro", "middle")}

    ${section(336, "Reduce node path", "PURE HOST COMPUTE · NO PROVIDER CALL")}
    ${card({ x: 48, y: 356, w: 250, h: 112, title: "Dependency outputs", body: ["Only completed upstream values", "Stable ordering from dependsOn"] })}
    ${arrow("M298 412H364", true)}
    ${card({ x: 366, y: 356, w: 300, h: 112, title: "Named reducer", body: ["concat · dedupe_lines · count", "merge_json_arrays · custom function"], accent: true })}
    ${arrow("M666 412H732", true)}
    ${card({ x: 734, y: 356, w: 418, h: 112, title: "Deterministic downstream value", body: ["Recorded as workflow-node output", "Can feed writer, verifier, or another reducer"] })}

    ${section(516, "Failure semantics", "INVALID OUTPUT NEVER SILENTLY BECOMES VALID DATA")}
    ${card({ x: 48, y: 536, w: 344, h: 98, title: "Retry succeeds", body: ["Validated value commits", "Dependents become ready"] })}
    ${card({ x: 428, y: 536, w: 344, h: 98, title: "Attempts exhausted", body: ["Node fails with validation evidence", "Dependent nodes remain blocked"] })}
    ${card({ x: 808, y: 536, w: 344, h: 98, title: "Reducer missing / throws", body: ["Reduce node fails explicitly", "No LLM fallback hides the fault"] })}
    ${section(682, "Ownership boundary", "SCHEDULING: KERNEL · INSTRUCTION / VALIDATION / REDUCER EXECUTION: HOST SDK")}
  `,
})

add("milestones_mechanisms.svg", {
  title: "DeepStrike milestone state machine",
  desc: "Milestone phases progress from pending through evaluation to passed or failed, collect required evidence, unlock later capabilities, and apply explicit retry or termination policy.",
  eyebrow: "MILESTONES",
  headline: "Acceptance gates turn long work into explicit phases.",
  subtitle: "CONTRACT · EVIDENCE · EVALUATION · UNLOCK",
  body: `
    ${section(124, "Milestone contract", "PHASES AND REQUIRED EVIDENCE ARE DATA")}
    ${card({ x: 48, y: 146, w: 250, h: 104, title: "Pending phase", body: ["id · description · criteria", "requiredEvidence · unlocks"], accent: true })}
    ${arrow("M298 198H370", true)}
    ${card({ x: 372, y: 146, w: 250, h: 104, title: "Evidence boundary", body: ["Agent emits milestone_check", "Kernel records supplied evidence"] })}
    ${arrow("M622 198H694", true)}
    ${card({ x: 696, y: 146, w: 250, h: 104, title: "Evaluation policy", body: ["require_verifier · terminate", "auto_pass for development"] })}
    ${arrow("M946 198H1018", true)}
    ${card({ x: 1020, y: 146, w: 132, h: 104, title: "Verdict", body: ["pass", "or fail"] })}

    ${section(300, "State transitions", "FAILURE POLICY IS EXPLICIT AND BOUNDED")}
    ${rect(48, 320, 1104, 156, "kernel", 7)}
    ${pill(82, 350, 92, "PENDING", "ivory")}
    ${arrow("M176 361H318")}
    ${pill(320, 350, 108, "EVALUATING")}
    ${arrow("M430 361H572", true)}
    ${pill(574, 350, 80, "PASSED", "ivory")}
    ${arrow("M654 361H796")}
    ${pill(798, 350, 96, "UNLOCKED", "ivory")}
    ${arrow("M374 372V430H574", true, true)}
    ${pill(576, 420, 76, "FAILED", "muted")}
    ${arrow("M652 431H758", true)}
    ${text(778, 435, "retry / rollback / terminate", "label")}
    ${text(82, 448, "Phase state and evidence are journaled; retry exhaustion terminates instead of re-entering an unbounded loop.", "body")}

    ${section(526, "Composition", "MILESTONES DO NOT REPLACE WORKFLOWS OR HARNESSES")}
    ${card({ x: 48, y: 546, w: 258, h: 92, title: "With workflows", body: ["Gate one run phase", "while DAG nodes schedule work"] })}
    ${card({ x: 324, y: 546, w: 258, h: 92, title: "With sub-agents", body: ["Verifier supplies evidence", "Unlock narrows later authority"] })}
    ${card({ x: 600, y: 546, w: 258, h: 92, title: "With harness eval", body: ["Verdict feedback can retry", "without changing kernel semantics"] })}
    ${card({ x: 876, y: 546, w: 276, h: 92, title: "With SessionLog", body: ["Attempts and outcomes survive", "replay, audit, and recovery"] })}
    ${section(684, "Invariant", "A PHASE PASSES ONLY FROM EVIDENCE ACCEPTED BY ITS CONFIGURED POLICY")}
  `,
})

add("context_vm_mechanisms.svg", {
  title: "DeepStrike Context VM",
  desc: "Four-slot context address space, prompt-cache boundaries, pressure accounting, compression, handle paging, and distinct knowledge, skill, and memory lifecycles.",
  eyebrow: "CONTEXT VM",
  headline: "Context is partitioned state, not an ever-growing chat log.",
  subtitle: "FOUR SLOTS · PRESSURE · COMPACTION · RESIDENCY",
  body: `
    ${section(124, "Address space rendered before every provider call", "STABLE PREFIX ON THE LEFT · TURN-LOCAL STATE ON THE RIGHT")}
    ${card({ x: 48, y: 146, w: 258, h: 126, title: "1 · system_stable", body: ["Identity + immutable directives", "Byte-stable prompt-cache target", "Changes invalidate the long prefix"], accent: true })}
    ${card({ x: 324, y: 146, w: 258, h: 126, title: "2 · system_knowledge", body: ["Skill bodies · initialMemory", "Host-pinned keyed references", "Budgeted + boundary-evicted"] })}
    ${card({ x: 600, y: 146, w: 258, h: 126, title: "3 · turns", body: ["Conversation + tool observations", "Memory / knowledge retrieval hits", "Frozen prefix + growing tail"] })}
    ${card({ x: 876, y: 146, w: 276, h: 126, title: "4 · state_turn", body: ["Task state · signals · directives", "Recomputed every turn", "Never treated as cache-stable"] })}
    ${rule(48, 292, 858, "rule-coral")}
    ${text(453, 312, "PROMPT-CACHE-AWARE STABLE / FROZEN REGION", "micro", "middle")}

    ${section(354, "Pressure and compaction", "ρ INCLUDES TEXT, IMAGES, AUDIO, TOOL RESULTS, AND RESERVES")}
    ${card({ x: 48, y: 374, w: 250, h: 108, title: "Pressure meter", body: ["TokenEngine estimates all slots", "Threshold and hard limit are policy"] })}
    ${arrow("M298 428H364", true)}
    ${card({ x: 366, y: 374, w: 300, h: 108, title: "Compression pyramid", body: ["Snip large results → drop old turns", "Summarize archive when required"], accent: true })}
    ${arrow("M666 428H732", true)}
    ${card({ x: 734, y: 374, w: 418, h: 108, title: "Renewal boundary", body: ["Re-query memory · sweep knowledge · expire leases", "Recompute cache generation and frozen prefix"] })}

    ${section(532, "Residency and content lifetimes", "LARGE VALUES USE HANDLES · DIFFERENT FACTS HAVE DIFFERENT LIFETIMES")}
    ${card({ x: 48, y: 552, w: 258, h: 102, title: "Large tool output", body: ["Inline preview + H# descriptor", "Payload pages to host spool"] })}
    ${card({ x: 324, y: 552, w: 258, h: 102, title: "Skill body", body: ["Key: skill:<name>", "Resident until deactivate / lease expiry"] })}
    ${card({ x: 600, y: 552, w: 258, h: 102, title: "Retrieval hit", body: ["Ordinary history turn", "Decays through compaction"] })}
    ${card({ x: 876, y: 552, w: 276, h: 102, title: "Pinned knowledge", body: ["Host-keyed + pinned", "Exempt from knowledge-budget eviction"] })}
    ${section(694, "Invariant", "EACH CONTENT CLASS ENTERS ONE SLOT WITH AN EXPLICIT EVICTION POLICY")}
  `,
})

add("execution_plane_mechanisms.svg", {
  title: "DeepStrike ExecutionPlane",
  desc: "Approved tool calls move through host decision hooks into local, worktree, sandbox, or remote execution, with streaming, suspension, result redaction, and large-result spooling.",
  eyebrow: "EXECUTION PLANE",
  headline: "The kernel grants authority; the host performs the effect.",
  subtitle: "TOOLS · STREAMING · ISOLATION · SUSPEND · SPOOL",
  body: `
    ${section(124, "Approved-call path", "NO TOOL FUNCTION RUNS INSIDE deepstrike-core")}
    ${card({ x: 48, y: 146, w: 220, h: 112, title: "KernelEffect", body: ["ExecuteTool(calls)", "Already passed kernel governance"], accent: true })}
    ${arrow("M268 202H314", true)}
    ${card({ x: 316, y: 146, w: 220, h: 112, title: "Host pre-hook", body: ["onToolCall for stateful policy", "Throw → fail closed by default"] })}
    ${arrow("M536 202H582", true)}
    ${card({ x: 584, y: 146, w: 220, h: 112, title: "ExecutionPlane", body: ["Lookup schema + implementation", "Validate / repair arguments"] })}
    ${arrow("M804 202H850", true)}
    ${card({ x: 852, y: 146, w: 300, h: 112, title: "Host post-hook + observation", body: ["Redact / replace output · inject note", "Return ToolResult to kernel + SessionLog"] })}

    ${section(306, "Execution backends", "THE SAME TOOL CONTRACT CAN RUN IN DIFFERENT TRUST ZONES")}
    ${card({ x: 48, y: 326, w: 258, h: 106, title: "Local", body: ["In-process functions", "RunContext carries cwd + signal"] })}
    ${card({ x: 324, y: 326, w: 258, h: 106, title: "Worktree", body: ["Per-agent git worktree cwd", "Lifecycle managed by host"] })}
    ${card({ x: 600, y: 326, w: 258, h: 106, title: "Process sandbox", body: ["Subprocess boundary", "Timeout · cancellation · limits"] })}
    ${card({ x: 876, y: 326, w: 276, h: 106, title: "Remote VPC / MCP proxy", body: ["Customer-network execution", "Protocol adapter stays host-side"] })}

    ${section(480, "Long-running and large-result paths", "STREAM PROGRESS WITHOUT FLOODING CONTEXT")}
    ${card({ x: 48, y: 500, w: 344, h: 116, title: "Streaming tool", body: ["Async chunks become progress events", "Final chunk closes the tool-call pair"] })}
    ${card({ x: 428, y: 500, w: 344, h: 116, title: "Suspend / resume", body: ["Tool yields a suspend token", "Host callback resolves external work"] })}
    ${card({ x: 808, y: 500, w: 344, h: 116, title: "LargeResultSpool", body: ["Inline preview + content handle", "Payload stored on disk / DB and paged"] })}
    ${section(666, "Cancellation boundary", "USER · DEADLINE · LEASE LOST · HOST SHUTDOWN COMPOSE INTO THE OPERATION SIGNAL")}
  `,
})

add("provider_routing_mechanisms.svg", {
  title: "DeepStrike provider routing",
  desc: "The kernel carries model hints and budgets while the host resolves concrete providers, protocols, endpoints, runtime policy, modality serialization, and replay compatibility.",
  eyebrow: "PROVIDER ROUTING",
  headline: "The kernel requests capability; the host chooses the vendor.",
  subtitle: "modelHint · providerFor · PROTOCOL · REPLAY",
  body: `
    ${section(124, "Routing decision", "API KEYS, ENDPOINTS, AND PROVIDER OBJECTS NEVER ENTER THE KERNEL")}
    ${card({ x: 48, y: 146, w: 250, h: 108, title: "Workflow node / run", body: ["role · modelHint · token budget", "Kernel carries hint for audit"], accent: true })}
    ${arrow("M298 200H364", true)}
    ${card({ x: 366, y: 146, w: 300, h: 108, title: "RuntimeRunner.providerFor", body: ["Resolve hint to provider instance", "undefined falls back to default provider"] })}
    ${arrow("M666 200H732", true)}
    ${card({ x: 734, y: 146, w: 418, h: 108, title: "Concrete host configuration", body: ["API key · base URL · region · RuntimePolicy", "Retry, timeout, protocol, and operation cancellation"] })}

    ${section(302, "Provider families and wires", "FACTORIES COLLAPSE VENDOR-SPECIFIC CONFIGURATION")}
    ${card({ x: 48, y: 322, w: 258, h: 108, title: "Native base providers", body: ["Anthropic · OpenAI Chat", "OpenAI Responses · Gemini"] })}
    ${card({ x: 324, y: 322, w: 258, h: 108, title: "Vendor factories", body: ["DeepSeek · Kimi · Qwen", "GLM · Minimax · Ollama"] })}
    ${card({ x: 600, y: 322, w: 258, h: 108, title: "Protocol selection", body: ["OpenAI-compatible or Anthropic", "Provider descriptor records the wire"] })}
    ${card({ x: 876, y: 322, w: 276, h: 108, title: "Custom provider", body: ["Implement LLMProvider", "Preserve stream + replay contract"] })}

    ${section(478, "Call and replay path", "PROVIDER OUTPUT IS NORMALIZED BEFORE IT BECOMES KERNEL INPUT")}
    ${rect(48, 498, 1104, 128, "panel-strong", 7)}
    ${text(68, 530, "RenderedContext + narrowed tools + typed attachments", "label")}
    ${arrow("M362 527H478", true)}
    ${text(498, 530, "vendor-native request / stream", "label")}
    ${arrow("M710 527H826", true)}
    ${text(846, 530, "provider_result + replay envelope", "label")}
    ${text(68, 568, "Live", "card-title")}
    ${text(122, 568, "Network call executes under host retry, timeout, and cancellation policy.", "card-text")}
    ${text(68, 598, "Replay", "card-title")}
    ${text(122, 598, "ReplayProvider feeds recorded protocol-shaped results without another network call.", "card-text")}
    ${section(676, "Typical role routing", "EXPLORE: THROUGHPUT · IMPLEMENT: TOOL RELIABILITY · VERIFY: REASONING · REDUCE: NO LLM")}
  `,
})

add("multimodal_mechanisms.svg", {
  title: "DeepStrike multimodal input path",
  desc: "Image and audio attachments enter as typed content parts, receive kernel token weighting and durable persistence, then serialize to provider-native formats or fail explicitly when unsupported.",
  eyebrow: "MULTIMODAL INPUT",
  headline: "Attachments survive pressure accounting, replay, and resume.",
  subtitle: "TYPED PARTS · TOKEN WEIGHT · VENDOR SERIALIZATION",
  body: `
    ${section(124, "Ingress and kernel contract", "run({ attachments }) WORKS ACROSS NODE.JS · PYTHON · RUST · WASM")}
    ${card({ x: 48, y: 146, w: 258, h: 112, title: "Image part", body: ["url or base64 data", "mediaType · detail: low / auto / high"], accent: true })}
    ${card({ x: 324, y: 146, w: 258, h: 112, title: "Audio part", body: ["base64 data + mediaType", "No URL form"] })}
    ${arrow("M582 202H648", true)}
    ${card({ x: 650, y: 146, w: 226, h: 112, title: "Content::Parts", body: ["Typed kernel message", "Persisted in run_started"] })}
    ${arrow("M876 202H942", true)}
    ${card({ x: 944, y: 146, w: 208, h: 112, title: "Context pressure", body: ["Image + audio have weight", "No modality is treated free"] })}

    ${section(306, "Token weighting", "DETERMINISTIC ESTIMATES FEED THE SAME ρ PRESSURE METER")}
    ${card({ x: 48, y: 326, w: 258, h: 94, title: "Image · low", body: ["85 tokens"] })}
    ${card({ x: 324, y: 326, w: 258, h: 94, title: "Image · auto", body: ["255 tokens"] })}
    ${card({ x: 600, y: 326, w: 258, h: 94, title: "Image · high", body: ["680 tokens"] })}
    ${card({ x: 876, y: 326, w: 276, h: 94, title: "Audio", body: ["approximately decoded bytes / 1600"] })}

    ${section(468, "Host serialization", "UNSUPPORTED MODALITY FAILS EXPLICITLY · NEVER SILENTLY DROPS INPUT")}
    ${card({ x: 48, y: 488, w: 210, h: 118, title: "Anthropic", body: ["image source block", "audio → UnsupportedModalityError"] })}
    ${card({ x: 274, y: 488, w: 210, h: 118, title: "OpenAI Chat", body: ["image_url", "input_audio: mp3 / wav"] })}
    ${card({ x: 500, y: 488, w: 210, h: 118, title: "OpenAI Responses", body: ["input_image", "audio unsupported"] })}
    ${card({ x: 726, y: 488, w: 210, h: 118, title: "Gemini", body: ["inlineData / fileData", "audio inlineData"] })}
    ${card({ x: 952, y: 488, w: 200, h: 118, title: "Ollama", body: ["images[]", "audio unsupported"] })}
    ${section(654, "Crash and resume", "SESSION RECONSTRUCTION RESTORES Content::Parts, NOT A TEXT-ONLY APPROXIMATION")}
    ${text(48, 686, "Attachments are durable run evidence and are rebuilt into the initial multimodal turn before provider replay or live continuation.", "body")}
  `,
})

add("memory_mechanisms.svg", {
  title: "DeepStrike memory lifecycle",
  desc: "Working, session, and durable memory paths, including kernel-validated writes and queries, prefetch and renewal recall, recall journaling, retention, promotion suggestions, and host-authoritative DreamStore state.",
  eyebrow: "MEMORY LIFECYCLE",
  headline: "Durable memory is a governed host device, not hidden context.",
  subtitle: "QUERY · VALIDATE · COMMIT · RECALL · RETAIN · PROMOTE",
  body: `
    ${section(124, "Three memory layers", "THEIR OWNERSHIP AND LIFETIMES ARE DIFFERENT")}
    ${card({ x: 48, y: 146, w: 344, h: 104, title: "Working", body: ["Scratch state for the current run", "No cross-session durability guarantee"] })}
    ${card({ x: 428, y: 146, w: 344, h: 104, title: "Session", body: ["Evidence events in SessionLog", "Auditable and recoverable"] })}
    ${card({ x: 808, y: 146, w: 344, h: 104, title: "Durable", body: ["DreamStore owns full record set", "Host decides retention, pinning, eviction"], accent: true })}

    ${section(298, "Recall path", "PREFETCH AND IN-RUN QUERY SHARE ONE JOURNALED ROUTE")}
    ${card({ x: 48, y: 318, w: 210, h: 112, title: "Run start / renewal", body: ["preQueryMemory(goal)", "Re-fires after context renewal"] })}
    ${card({ x: 274, y: 318, w: 210, h: 112, title: "Scoped query", body: ["Kernel validates memory scope", "DreamStore.search(topK)"] })}
    ${card({ x: 500, y: 318, w: 210, h: 112, title: "Rank + recall", body: ["Host ranks records", "memory_recalled increments count"] })}
    ${card({ x: 726, y: 318, w: 210, h: 112, title: "Context placement", body: ["Hits enter turns history", "Single-use and compressible"] })}
    ${card({ x: 952, y: 318, w: 200, h: 112, title: "Promotion signal", body: ["Threshold crossed", "Host decides whether to pin"] })}
    ${arrow("M258 374H272")}${arrow("M484 374H498")}${arrow("M710 374H724")}${arrow("M936 374H950", true)}

    ${section(478, "Write and retention path", "EVERY WRITE PASSES VALIDATION AND RESOURCE QUOTA")}
    ${card({ x: 48, y: 498, w: 258, h: 112, title: "write_memory", body: ["Content + metadata + scope", "Kernel validation + write-rate quota"], accent: true })}
    ${arrow("M306 554H372", true)}
    ${card({ x: 374, y: 498, w: 258, h: 112, title: "Host commit", body: ["Dedup + durable DreamStore write", "memory_written evidence"] })}
    ${arrow("M632 554H698", true)}
    ${card({ x: 700, y: 498, w: 208, h: 112, title: "Idle pipeline", body: ["Extract + consolidate", "Dream after session boundary"] })}
    ${arrow("M908 554H974", true)}
    ${card({ x: 976, y: 498, w: 176, h: 112, title: "Retention", body: ["Recall-aware score", "Host evicts / pins"] })}
    ${section(660, "Invariant", "DREAMSTORE IS AUTHORITATIVE · RETRIEVAL HITS DECAY · PROMOTION IS ADVISORY")}
    ${text(48, 690, "A recalled fact becomes durable knowledge only when the host explicitly promotes or pins it; the kernel never silently upgrades authority.", "body")}
  `,
})

add("skills_mechanisms.svg", {
  title: "DeepStrike skills, knowledge, and capability gating",
  desc: "Skill metadata enters the stable catalog, bodies load on demand into keyed knowledge, activation narrows tools by intersection, leases expire at boundaries, and manifests can only narrow the host baseline.",
  eyebrow: "SKILLS & CAPABILITIES",
  headline: "Load expertise on demand and narrow authority with it.",
  subtitle: "CATALOG · ACTIVATE · KNOWLEDGE · INTERSECTION · LEASE",
  body: `
    ${section(124, "Catalog and activation", "ONLY METADATA IS RESIDENT BEFORE THE MODEL CHOOSES A SKILL")}
    ${card({ x: 48, y: 146, w: 258, h: 112, title: "skillDir scan", body: ["Frontmatter: name · description", "allowedTools · effort · token estimate"] })}
    ${arrow("M306 202H372", true)}
    ${card({ x: 374, y: 146, w: 258, h: 112, title: "Stable catalog", body: ["Metadata only in context", "skill meta-tool selects by name"], accent: true })}
    ${arrow("M632 202H698", true)}
    ${card({ x: 700, y: 146, w: 208, h: 112, title: "Activation", body: ["Body returned once", "Kernel records active skill"] })}
    ${arrow("M908 202H974", true)}
    ${card({ x: 976, y: 146, w: 176, h: 112, title: "Knowledge pin", body: ["key: skill:<name>", "lease tracked"] })}

    ${section(306, "Capability intersection", "EVERY LAYER CAN NARROW · NO LAYER CAN WIDEN ITS BASELINE")}
    ${rect(48, 326, 1104, 130, "kernel", 7)}
    ${text(68, 357, "Registered tools", "label")}
    ${text(200, 357, "∩ host allowedToolIds", "label")}
    ${text(382, 357, "∩ HarnessManifest ids", "label")}
    ${text(570, 357, "∩ skillFilter / allowedTools", "label")}
    ${text(786, 357, "∪ stableCore + meta-tools", "label")}
    ${arrow("M68 382H1080", true)}
    ${text(68, 420, "Effective tool schema", "card-title")}
    ${text(218, 420, "is the safe subset exposed on the next provider call; an empty tool intersection throws instead of widening.", "card-text")}

    ${section(504, "Lease and boundary lifecycle", "ACTIVATION IS AN EPOCH EVENT BECAUSE IT CHANGES THE PROMPT CACHE")}
    ${card({ x: 48, y: 524, w: 258, h: 106, title: "Active", body: ["Tool surface narrowed", "Body resident in knowledge"] })}
    ${card({ x: 324, y: 524, w: 258, h: 106, title: "Refresh", body: ["Repeat skill(name)", "resets lease turn count"] })}
    ${card({ x: 600, y: 524, w: 258, h: 106, title: "Deactivate / expire", body: ["Host command or lease end", "Tool surface re-widens to baseline"] })}
    ${card({ x: 876, y: 524, w: 276, h: 106, title: "Boundary sweep", body: ["Drop skill knowledge pin", "Advance cache generation"] })}
    ${arrow("M306 577H322")}${arrow("M582 577H598")}${arrow("M858 577H874", true)}
    ${section(678, "Separation", "SKILL = METHOD · KNOWLEDGE = PINNED REFERENCE · MEMORY HIT = DECAYING FACT")}
  `,
})

add("session_replay_mechanisms.svg", {
  title: "DeepStrike session replay and recovery",
  desc: "The append-only SessionLog evidence stream records provider, tool, governance, workflow, memory, multimodal, and lifecycle events for audit, provider replay, workflow resume, repair, and OS snapshots.",
  eyebrow: "SESSION EVIDENCE",
  headline: "Recovery folds durable observations instead of guessing state.",
  subtitle: "APPEND-ONLY LOG · REPLAY · REPAIR · RESUME",
  body: `
    ${section(124, "Live evidence stream", "HOST APPENDS EVENTS AFTER SEMANTIC BOUNDARIES COMMIT")}
    ${card({ x: 48, y: 146, w: 208, h: 116, title: "Run lifecycle", body: ["run_started + attachments", "suspended · resumed · terminal"], accent: true })}
    ${card({ x: 272, y: 146, w: 208, h: 116, title: "Provider", body: ["text · tool calls", "protocol-shaped replay envelope"] })}
    ${card({ x: 496, y: 146, w: 208, h: 116, title: "Tools & governance", body: ["requested · gated · completed", "permission request + resolution"] })}
    ${card({ x: 720, y: 146, w: 208, h: 116, title: "Workflow & process", body: ["node output · submissions", "agent_process_changed lineage"] })}
    ${card({ x: 944, y: 146, w: 208, h: 116, title: "Memory & context", body: ["write · query · recall", "compression · renewal · spool"] })}

    ${section(310, "Four consumers of the same evidence", "ONE LOG · DIFFERENT FOLDS")}
    ${card({ x: 48, y: 330, w: 258, h: 126, title: "Audit & primitive view", body: ["Filter by category / kernel primitive", "Explain who requested and who executed", "No claim of filesystem rollback"] })}
    ${card({ x: 324, y: 330, w: 258, h: 126, title: "Provider replay", body: ["ReplayProvider reads envelopes", "No live network or model tokens", "Validate deterministic host behavior"] })}
    ${card({ x: 600, y: 330, w: 258, h: 126, title: "Workflow resume", body: ["Fold completed node outputs", "Restore runtime submissions", "Schedule remaining frontier"] })}
    ${card({ x: 876, y: 330, w: 276, h: 126, title: "OS Snapshot", body: ["Fold processes · budgets · signals", "Permissions · paging · memory", "Dashboard state, not restore state"] })}

    ${section(504, "Kernel recovery path", "PORTABLE SNAPSHOT REPLAYS ACCEPTED PUBLIC ABI TRANSACTIONS")}
    ${rect(48, 524, 1104, 118, "kernel", 7)}
    ${text(68, 554, "Session / transaction stream", "label")}
    ${arrow("M258 551H356", true)}
    ${text(376, 554, "validate + repair", "label")}
    ${arrow("M500 551H598", true)}
    ${text(618, 554, "fold public ABI", "label")}
    ${arrow("M742 551H840", true)}
    ${text(860, 554, "restore lifecycle / operation / effect ids", "label")}
    ${text(68, 602, "Malformed events can be sanitized or rejected; bounded snapshot journals fail explicitly instead of emitting partial checkpoints.", "body")}
    ${section(686, "Boundary", "SESSIONLOG RECOVERS CONTROL STATE · TOOL SIDE EFFECTS STILL REQUIRE IDEMPOTENCY OR COMPENSATION")}
  `,
})

add("snapshots_mechanisms.svg", {
  title: "DeepStrike OS profiles and snapshots",
  desc: "OS Profile configures validated runtime policy; OS Snapshot folds observable SessionLog state; KernelSnapshot restores exact accepted ABI state; ContextSnapshot restores context partitions only.",
  eyebrow: "PROFILES & SNAPSHOTS",
  headline: "Configuration, observability, and recovery are different artifacts.",
  subtitle: "PROFILE ≠ OS SNAPSHOT ≠ KERNEL SNAPSHOT",
  body: `
    ${section(124, "Before the run", "PROFILE SELECTS POLICY · VALIDATION FAILS BEFORE STARTUP")}
    ${card({ x: 48, y: 146, w: 344, h: 112, title: "OS Profile", body: ["SignalPolicy + GovernancePolicy defaults", "Host-selectable native or custom profile", "Not a complete production safety boundary"], accent: true })}
    ${arrow("M392 202H456", true)}
    ${card({ x: 458, y: 146, w: 284, h: 112, title: "Declarative validation", body: ["Actions · patterns · queue bounds · TTL", "Invalid policy never reaches the kernel"] })}
    ${arrow("M742 202H806", true)}
    ${card({ x: 808, y: 146, w: 344, h: 112, title: "Runtime configuration", body: ["ConfigureRun + granular updates", "Compose profile with ResourceQuota", "KernelReliability bounds recovery"] })}

    ${section(306, "After or during the run", "CHOOSE THE ARTIFACT FOR THE QUESTION YOU ARE ASKING")}
    ${card({ x: 48, y: 326, w: 344, h: 146, title: "OS Snapshot · observe", body: ["Folded from SessionLog events", "Processes · budget · signals · paging", "Permissions · spool · memory counters", "Cannot restore execution"], accent: true })}
    ${card({ x: 428, y: 326, w: 344, h: 146, title: "KernelSnapshot · restore", body: ["Accepted ABI transaction journal", "Lifecycle + operation + effect identity", "Strict single-version ABI v2", "Bounded; incompatible fails explicitly"] })}
    ${card({ x: 808, y: 326, w: 344, h: 146, title: "ContextSnapshot · context only", body: ["Four context partitions", "Token and cache metadata", "Partial restore surface", "Not process / workflow recovery"] })}

    ${section(520, "Dashboard fold", "OS SNAPSHOT MAKES KERNEL PRIMITIVES OPERATIONALLY VISIBLE")}
    ${rect(48, 540, 1104, 104, "panel-strong", 7)}
    ${text(68, 570, "health", "label")}${text(178, 570, "queue", "label")}${text(288, 570, "permissions", "label")}${text(430, 570, "process tree", "label")}${text(580, 570, "budget", "label")}${text(690, 570, "signals", "label")}${text(800, 570, "paging / spool", "label")}${text(972, 570, "memory", "label")}
    ${rule(68, 590, 1132, "rule-coral")}
    ${text(68, 620, "session_log_has_required_categories verifies category and primitive completeness before dashboard ingest.", "body")}
    ${section(692, "Production rule", "PROFILE SETS BOUNDARIES · QUOTA LIMITS RESOURCES · SNAPSHOT REPORTS OR RESTORES STATE")}
  `,
})

add("reliability_mechanisms.svg", {
  title: "DeepStrike runtime reliability mechanisms",
  desc: "Bounded input and snapshot journals, replay deduplication, provider and durability recovery attempts, cancellation, repeat fuse, criteria gate, entropy watch, and explicit terminal reasons.",
  eyebrow: "RUNTIME RELIABILITY",
  headline: "Every recovery path has a bound and an observable outcome.",
  subtitle: "LIMITS · RETRIES · FUSES · CANCELLATION · EVIDENCE",
  body: `
    ${section(124, "ABI and durability bounds", "STRICT INPUTS PREVENT SILENTLY PARTIAL RECOVERY")}
    ${card({ x: 48, y: 146, w: 258, h: 116, title: "Input validation", body: ["Canonical JSON byte limit", "Unknown ABI v2 fields rejected"] })}
    ${card({ x: 324, y: 146, w: 258, h: 116, title: "Replay windows", body: ["Input-event dedupe capacity", "Completed-effect replay capacity"] })}
    ${card({ x: 600, y: 146, w: 258, h: 116, title: "Snapshot journal", body: ["Input-count + byte bounds", "Overflow → snapshot_incompatible"], accent: true })}
    ${card({ x: 876, y: 146, w: 276, h: 116, title: "Durability retry", body: ["Host effect retry attempts", "Best-effort failures surface to handler"] })}

    ${section(310, "Loop recovery and termination", "NO UNBOUNDED RETRY LADDER")}
    ${card({ x: 48, y: 330, w: 210, h: 126, title: "Provider recovery", body: ["Context-overflow attempts", "Truncated-output attempts", "Both capped 0..16"] })}
    ${card({ x: 274, y: 330, w: 210, h: 126, title: "Repeat fuse", body: ["Digest full tool arguments", "Deny repeated call", "Terminate persistent no-progress"] })}
    ${card({ x: 500, y: 330, w: 210, h: 126, title: "Criteria gate", body: ["One verification turn", "before accepting completion", "Does not loop forever"] })}
    ${card({ x: 726, y: 330, w: 210, h: 126, title: "Entropy watch", body: ["Per-turn sample", "Hysteresis + cooldown", "Optional signal to model"] })}
    ${card({ x: 952, y: 330, w: 200, h: 126, title: "Budget funnel", body: ["Turns · tokens · wall time", "Checked before provider call", "Final report bounded"] })}

    ${section(504, "Operation cancellation", "ONE SIGNAL COMPOSES HOST AND USER TERMINAL CONDITIONS")}
    ${rect(48, 524, 1104, 106, "kernel", 7)}
    ${pill(78, 552, 74, "USER", "ivory")}${pill(196, 552, 92, "DEADLINE", "ivory")}${pill(332, 552, 104, "LEASE LOST", "ivory")}${pill(480, 552, 128, "HOST SHUTDOWN", "ivory")}
    ${arrow("M628 563H784", true)}
    ${pill(806, 552, 148, "ABORT OPERATION")}
    ${arrow("M974 563H1080", true)}
    ${text(1098, 567, "Done", "label")}
    ${text(78, 608, "Cancellation, quota, no-progress, invalid config, and recovery exhaustion each produce an explicit terminal reason in evidence.", "body")}
    ${section(684, "Invariant", "RETRY ONLY WHEN THE POLICY NAMES A LIMIT · FAIL CLOSED WHEN RECOVERY CANNOT PROVE STATE")}
  `,
})

add("signals_mechanisms.svg", {
  title: "DeepStrike signals and reactive sessions",
  desc: "Signals enter through scheduled prompts, webhooks, injected notes, or broadcasts, use leased claim and acknowledgment, receive kernel attention dispositions, then feed a reactive multi-peer session with shared budget and idempotent checkpoints.",
  eyebrow: "SIGNALS & REACTIVE",
  headline: "Signals turn external events into governed peer work.",
  subtitle: "INGEST · LEASE · ATTENTION · BLACKBOARD · RUNGROUP",
  body: `
    ${section(124, "Signal ingress and delivery", "A DURABLE SOURCE MAY IMPLEMENT THE SAME LEASED CONTRACT")}
    ${card({ x: 48, y: 146, w: 210, h: 116, title: "Ingress", body: ["schedule · ingest · injectNote", "broadcast · targeted interrupt"], accent: true })}
    ${arrow("M258 204H304", true)}
    ${card({ x: 306, y: 146, w: 210, h: 116, title: "Recipient + dedupe", body: ["recipient routing", "dedupe key + TTL / deadline"] })}
    ${arrow("M516 204H562", true)}
    ${card({ x: 564, y: 146, w: 210, h: 116, title: "Claim lease", body: ["claim → ack on kernel accept", "nack / expiry makes visible again"] })}
    ${arrow("M774 204H820", true)}
    ${card({ x: 822, y: 146, w: 330, h: 116, title: "Attention disposition", body: ["ignore · observe · queue · run", "interrupt · interrupt_now · dropped"] })}

    ${section(310, "Kernel attention plane", "SIGNALS RENDER IN state_turn, NEVER IN THE CACHE-STABLE PREFIX")}
    ${rect(48, 330, 1104, 118, "kernel", 7)}
    ${text(68, 361, "Urgency + task lifecycle + deadline escalation", "label")}
    ${arrow("M346 358H466", true)}
    ${text(486, 361, "bounded queue / displacement / dedupe", "label")}
    ${arrow("M716 358H836", true)}
    ${text(856, 361, "[SIGNAL] directive on a turn boundary", "label")}
    ${text(68, 410, "Queue overflow returns dropped without committing the dedupe key, so the SDK can apply backpressure and retry safely.", "body")}

    ${section(496, "ReactiveSession governance domain", "BLACKBOARD SELECTS WORK · RUNGROUP ACCOUNTS FOR IT")}
    ${card({ x: 48, y: 516, w: 210, h: 116, title: "EventStream", body: ["Shared blackboard", "visibility by channel / audience"] })}
    ${card({ x: 274, y: 516, w: 210, h: 116, title: "TurnPolicy", body: ["Select visible peers", "reactByMention or custom"] })}
    ${card({ x: 500, y: 516, w: 210, h: 116, title: "Reaction checkpoint", body: ["Idempotency key + lease", "Save plan and partial outputs"] })}
    ${card({ x: 726, y: 516, w: 210, h: 116, title: "Peer RuntimeRunner", body: ["One durable session per persona", "Retry only unfinished reactions"] })}
    ${card({ x: 952, y: 516, w: 200, h: 116, title: "RunGroup", body: ["Shared cumulative budget", "Membership + lineage"] })}
    ${arrow("M258 574H272")}${arrow("M484 574H498")}${arrow("M710 574H724")}${arrow("M936 574H950", true)}
    ${section(684, "Deployment boundary", "IN-MEMORY STORES ARE PROCESS-LOCAL · MULTI-REPLICA USES ATOMIC DURABLE IMPLEMENTATIONS")}
  `,
})

add("collaboration_mechanisms.svg", {
  title: "DeepStrike sub-agent collaboration and isolation",
  desc: "Parent-child TCB lineage, role and context inheritance, capability subsets, shared, read-only, worktree, and remote isolation, contracts, handoff artifacts, and RunGroup settlement.",
  eyebrow: "SUB-AGENTS",
  headline: "Delegation creates a child process with less or equal authority.",
  subtitle: "IDENTITY · CONTEXT · CAPABILITY · ISOLATION · HANDOFF",
  body: `
    ${section(124, "Spawn contract", "THE CHILD BECOMES A TCB IN THE SAME TASKTABLE")}
    ${card({ x: 48, y: 146, w: 258, h: 120, title: "Parent RuntimeRunner", body: ["AgentRunSpec or workflow node", "Parent session + RunGroup identity"], accent: true })}
    ${arrow("M306 206H372", true)}
    ${card({ x: 374, y: 146, w: 258, h: 120, title: "Syscall::Spawn", body: ["Governance + ResourceQuota", "Depth · concurrency · total count"] })}
    ${arrow("M632 206H698", true)}
    ${card({ x: 700, y: 146, w: 208, h: 120, title: "Child TCB", body: ["Role · lifecycle · budget", "Parent waits on join"] })}
    ${arrow("M908 206H974", true)}
    ${card({ x: 976, y: 146, w: 176, h: 120, title: "Result", body: ["Termination", "output + artifact", "durable lineage"] })}

    ${section(314, "Authority and context inheritance", "CAPABILITIES ARE SUBSETS · QUARANTINE CANNOT ESCALATE")}
    ${card({ x: 48, y: 334, w: 258, h: 112, title: "Context: none", body: ["Fresh system / task context", "Best for independent verifier"] })}
    ${card({ x: 324, y: 334, w: 258, h: 112, title: "Context: system_only", body: ["Identity and stable directives", "No parent conversation history"] })}
    ${card({ x: 600, y: 334, w: 258, h: 112, title: "Context: full", body: ["Explicit parent context inheritance", "Still bounded by child policy"] })}
    ${card({ x: 876, y: 334, w: 276, h: 112, title: "Capability filter", body: ["inherit or filtered tool access", "effective child set ⊆ parent set"], accent: true })}

    ${section(494, "Execution isolation modes", "HOST MAPS THE MANIFEST TO A CONCRETE EXECUTION PLANE")}
    ${card({ x: 48, y: 514, w: 210, h: 108, title: "shared", body: ["Parent plane / cwd", "Fastest · least isolation"] })}
    ${card({ x: 274, y: 514, w: 210, h: 108, title: "read_only", body: ["Explore without writes", "Filtered capability surface"] })}
    ${card({ x: 500, y: 514, w: 210, h: 108, title: "worktree", body: ["Dedicated git worktree cwd", "Host creates and removes"] })}
    ${card({ x: 726, y: 514, w: 210, h: 108, title: "remote", body: ["VPC or sandbox plane", "Credentials stay remote / host"] })}
    ${card({ x: 952, y: 514, w: 200, h: 108, title: "quarantined", body: ["Deny-all tools", "Taint propagates to children"] })}
    ${section(670, "Join boundary", "CONTRACT + HANDOFF ARTIFACT TURN CHILD OUTPUT INTO PARENT-CONSUMABLE EVIDENCE")}
    ${text(48, 700, "Nested children join RunGroup lineage and settle actual usage, while local kernel budgets and ResourceQuota remain the child safety boundary.", "body")}
  `,
})

add("harness_eval_mechanisms.svg", {
  title: "DeepStrike harness and evaluation",
  desc: "Ordinary agent runs can be wrapped by single-pass, retry, eval-loop, or contract-driven harnesses using an independent judge, structured verdicts, bounded feedback, workflow integration, and SessionLog evidence.",
  eyebrow: "HARNESS & EVAL",
  headline: "Evaluation wraps the run without replacing the runtime loop.",
  subtitle: "RUN · JUDGE · FEEDBACK · BOUNDED RETRY · VERDICT",
  body: `
    ${section(124, "One evaluation attempt", "GENERATOR AND JUDGE MAY USE DIFFERENT PROVIDERS")}
    ${card({ x: 48, y: 146, w: 258, h: 116, title: "Agent run", body: ["Normal RuntimeRunner semantics", "Tools · governance · SessionLog"], accent: true })}
    ${arrow("M306 204H372", true)}
    ${card({ x: 374, y: 146, w: 258, h: 116, title: "Candidate output", body: ["Text or structured value", "Attempt evidence stays durable"] })}
    ${arrow("M632 204H698", true)}
    ${card({ x: 700, y: 146, w: 208, h: 116, title: "Judge", body: ["Independent eval provider", "Criterion-by-criterion verdict"] })}
    ${arrow("M908 204H974", true)}
    ${card({ x: 976, y: 146, w: 176, h: 116, title: "Verdict", body: ["pass · score", "feedback", "evidence"] })}

    ${section(310, "Bounded harness loop", "FAILURE FEEDBACK BECOMES THE NEXT ATTEMPT'S INPUT")}
    ${rect(48, 330, 1104, 136, "kernel", 7)}
    ${pill(78, 354, 90, "ATTEMPT", "ivory")}
    ${arrow("M170 365H314", true)}
    ${pill(316, 354, 82, "JUDGE")}
    ${arrow("M400 365H544", true)}
    ${pill(546, 354, 78, "PASS", "ivory")}
    ${arrow("M624 365H768")}
    ${pill(770, 354, 82, "RETURN", "ivory")}
    ${arrow("M356 376V430H142V378", true, true)}
    ${text(248, 426, "fail → bounded feedback + retry", "micro", "middle")}
    ${text(78, 448, "maxAttempts and contract policy cap retries; the harness cannot silently create an unbounded agent loop.", "body")}

    ${section(514, "Harness forms and composition", "ALL OF THEM REUSE THE SAME RUNNER, WORKFLOW, AND EVIDENCE CONTRACTS")}
    ${card({ x: 48, y: 534, w: 258, h: 102, title: "SinglePassHarness", body: ["One run + one eval", "Fast acceptance check"] })}
    ${card({ x: 324, y: 534, w: 258, h: 102, title: "HarnessLoop", body: ["Feedback-driven attempts", "Bounded by maxAttempts"] })}
    ${card({ x: 600, y: 534, w: 258, h: 102, title: "ContractDrivenHarness", body: ["Collaboration criteria", "Handoff / milestone evidence"] })}
    ${card({ x: 876, y: 534, w: 276, h: 102, title: "Workflow node harness", body: ["Per-node retry / verifier", "Dependents see accepted output only"] })}
    ${section(686, "Separation", "HARNESS EVAL IMPROVES ONE OUTPUT · SELF-HARNESS EVOLVES THE NEXT RUN'S BOUNDED PROFILE")}
  `,
})

add("self_harness_mechanisms.svg", {
  title: "DeepStrike Self-Harness v2",
  desc: "Verifier-anchored failure evidence is isolated by scope, clustered, mined, patched only through whitelisted surfaces, screened by promotion tier, validated on held-in and held-out tasks, human-vetoed, and persisted as content-addressed lineage.",
  eyebrow: "SELF-HARNESS v2",
  headline: "Improve the harness without letting evidence rewrite authority.",
  subtitle: "SCOPE · WHITELIST · SCREEN · HELD-OUT · TIERED PROMOTION",
  body: `
    ${section(124, "Evidence and proposal lane", "HELD-OUT TASK CONTENT NEVER ENTERS THE MINER OR PROPOSER")}
    ${card({ x: 48, y: 146, w: 210, h: 126, title: "Failure evidence", body: ["Verifier-anchored records", "Trace excerpts marked as data", "Tool usage by failure cluster"], accent: true })}
    ${arrow("M258 210H304", true)}
    ${card({ x: 306, y: 146, w: 210, h: 126, title: "Scope isolation", body: ["One tenant / agent-group lane", "Mixed-scope bundle throws", "scope × modelProfile lineage"] })}
    ${arrow("M516 210H562", true)}
    ${card({ x: 564, y: 146, w: 210, h: 126, title: "Mine mechanism", body: ["Cluster by failure signature", "Addressable causes only", "No per-task patching"] })}
    ${arrow("M774 210H820", true)}
    ${card({ x: 822, y: 146, w: 330, h: 126, title: "Propose minimal HarnessPatch", body: ["Exactly one editable surface", "Bound to targetCluster", "Canonical JSON + parent-linked digest"] })}

    ${section(320, "Safety boundary", "THE MANIFEST CAN TUNE BEHAVIOR OR NARROW CAPABILITY · NEVER WIDEN IT")}
    ${card({ x: 48, y: 340, w: 258, h: 126, title: "Editable whitelist", body: ["instructions.* · nudges", "typed runtime knobs", "memory retrieval / promotion knobs"] })}
    ${card({ x: 324, y: 340, w: 258, h: 126, title: "Capability ceiling", body: ["allowedToolIds · stableCoreToolIds", "enablePlanTool · skillFilter", "effective = manifest ∩ host baseline"], accent: true })}
    ${card({ x: 600, y: 340, w: 258, h: 126, title: "Promotion tier", body: ["Tier A typed knobs: auto", "Tier B free text: screened", "Tier C widening: human only"] })}
    ${card({ x: 876, y: 340, w: 276, h: 126, title: "Injection screen", body: ["Runs before evaluation spend", "Any suspicious flag rejects", "Unparseable verdict fails closed"] })}

    ${section(514, "Validate and promote", "ACCEPT ONLY NON-REGRESSING CHANGE ON BOTH SPLITS")}
    ${rect(48, 534, 1104, 106, "panel-strong", 7)}
    ${text(68, 565, "held-in Δ ≥ 0", "label")}
    ${text(216, 565, "+", "label")}
    ${text(252, 565, "held-out Δ ≥ 0", "label")}
    ${text(420, 565, "+", "label")}
    ${text(456, 565, "at least one Δ > 0", "label")}
    ${arrow("M620 562H728", true)}
    ${text(748, 565, "onPromotionDecision", "label")}
    ${arrow("M906 562H1014", true)}
    ${text(1034, 565, "persist", "label")}
    ${text(68, 608, "Every promoted manifest stores parent digest, round, rationale, tier, screen verdict, split deltas, and target cluster.", "body")}
    ${section(684, "Shared layer", "SIGNATURE-ONLY AGGREGATE · ≥2 SCOPES · EXPLICIT HUMAN APPROVAL · NO AUTO PATH")}
  `,
})

await mkdir(OUT, { recursive: true })
for (const [name, svg] of diagrams) {
  await writeFile(path.join(OUT, name), svg.replace(/[ \t]+$/gm, ""), "utf8")
}

console.log(`generated ${diagrams.size} architecture SVGs in ${path.relative(ROOT, OUT)}`)
