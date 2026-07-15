/** Shared nav/sidebar definitions for zh (root) and en locales. */

type SidebarItem = { text: string; link: string }
type SidebarGroup = { text: string; items: SidebarItem[] }

function sidebar(prefix: '' | '/en'): SidebarGroup[] {
  const p = prefix
  return [
    {
      text: prefix ? 'Getting Started' : '入门',
      items: [
        { text: prefix ? 'Introduction' : '简介', link: `${p}/getting-started/` },
        { text: prefix ? 'Installation' : '安装', link: `${p}/getting-started/installation` },
        { text: 'Hello Agent', link: `${p}/getting-started/hello-agent` },
        { text: prefix ? 'Choosing an API' : 'API 选型', link: `${p}/getting-started/run-agent-vs-runner` },
        { text: prefix ? 'Providers' : 'Provider', link: `${p}/getting-started/providers` },
      ],
    },
    {
      text: prefix ? 'Architecture' : '架构',
      items: [
        { text: prefix ? 'Overview' : '总览', link: `${p}/architecture/` },
        { text: prefix ? 'What is Agent OS?' : '什么是 Agent OS', link: `${p}/architecture/agent-os` },
        { text: prefix ? 'Kernel / Host Split' : '内核与宿主分层', link: `${p}/architecture/overview` },
        { text: prefix ? 'Execution Model' : '执行模型', link: `${p}/architecture/execution-model` },
        { text: 'Kernel ABI', link: `${p}/architecture/kernel-abi` },
        { text: prefix ? 'Session & Replay' : 'Session 与重放', link: `${p}/architecture/session-replay` },
      ],
    },
    {
      text: prefix ? 'Guides' : '功能指南',
      items: [
        { text: prefix ? 'Guide Index' : '指南索引', link: `${p}/guides/` },
        { text: prefix ? 'Execution Plane & Tools' : '执行平面与工具', link: `${p}/guides/execution-plane-and-tools` },
        { text: prefix ? 'Context Engineering' : 'Context 工程', link: `${p}/guides/context-engineering` },
        { text: 'Skill', link: `${p}/guides/skills` },
        { text: 'Memory', link: `${p}/guides/memory` },
        { text: prefix ? 'Dynamic Workflows' : '动态工作流', link: `${p}/guides/workflow` },
        { text: prefix ? 'Structured Output & Reducers' : '结构化输出与 Reducer', link: `${p}/guides/structured-output-and-reducers` },
        { text: 'Governance', link: `${p}/guides/governance` },
        { text: prefix ? 'Provider Routing' : 'Provider 路由', link: `${p}/guides/provider-routing` },
        { text: prefix ? 'Multimodal Input' : '多模态输入', link: `${p}/guides/multimodal` },
        { text: prefix ? 'Session, Replay & Recovery' : 'Session、Replay 与恢复', link: `${p}/guides/session-replay-and-recovery` },
        { text: prefix ? 'OS Profile & Runtime Snapshots' : 'OS Profile 与运行时快照', link: `${p}/guides/os-profile-and-snapshots` },
        { text: prefix ? 'Signals & Reactive' : 'Signals 与 Reactive', link: `${p}/guides/signals-and-reactive` },
        { text: prefix ? 'Sub-Agents & Collaboration' : 'Sub-Agent 与协作', link: `${p}/guides/sub-agents-and-collaboration` },
        { text: prefix ? 'Harness & Eval' : 'Harness 与 Eval', link: `${p}/guides/harness-and-eval` },
        { text: 'Milestones', link: `${p}/guides/milestones` },
      ],
    },
    {
      text: prefix ? 'Concepts' : '概念',
      items: [
        { text: prefix ? 'Concept Index' : '概念索引', link: `${p}/concepts/` },
        { text: prefix ? 'Roles & Isolation' : '角色与隔离', link: `${p}/concepts/roles-and-isolation` },
        { text: prefix ? 'Prompt Cache Design' : 'Prompt Cache 设计', link: `${p}/concepts/prompt-cache-design` },
        { text: prefix ? 'RunGroup Budget' : 'RunGroup 预算', link: `${p}/concepts/run-group-budget` },
      ],
    },
    {
      text: prefix ? 'Reference' : '参考',
      items: [
        { text: prefix ? 'Reference Index' : '参考索引', link: `${p}/reference/` },
        { text: 'RuntimeOptions', link: `${p}/reference/runtime-options` },
        { text: 'WorkflowNodeSpec', link: `${p}/reference/workflow-node-spec` },
        { text: prefix ? 'Python API' : 'Python API', link: `${p}/reference/python-api` },
      ],
    },
  ]
}

export const zhSidebar = sidebar('')
export const enSidebar = sidebar('/en')

export const zhNav = [
  { text: '首页', link: '/' },
  { text: '架构', link: '/architecture/' },
  { text: '快速开始', link: '/getting-started/hello-agent' },
  { text: '功能指南', link: '/guides/' },
  { text: '参考', link: '/reference/' },
  { text: 'Wiki', link: 'https://github.com/kongusen/deepstrike/wiki' },
]

export const enNav = [
  { text: 'Home', link: '/en/' },
  { text: 'Architecture', link: '/en/architecture/' },
  { text: 'Quick Start', link: '/en/getting-started/hello-agent' },
  { text: 'Guides', link: '/en/guides/' },
  { text: 'Reference', link: '/en/reference/' },
  { text: 'Wiki', link: 'https://github.com/kongusen/deepstrike/wiki' },
]
