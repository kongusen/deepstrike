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
      text: prefix ? 'Agent Capabilities' : 'Agent 能力指南',
      items: [
        { text: prefix ? 'Capability Index' : '能力索引', link: `${p}/guides/` },
        { text: prefix ? 'Models & Providers' : '模型与 Provider', link: `${p}/guides/provider-routing` },
        { text: prefix ? 'Tools & Integrations' : '工具与集成', link: `${p}/guides/execution-plane-and-tools` },
        { text: prefix ? 'Skills & Knowledge' : 'Skill 与 Knowledge', link: `${p}/guides/skills` },
        { text: prefix ? 'Memory' : 'Memory', link: `${p}/guides/memory` },
        { text: prefix ? 'Context Management' : 'Context 管理', link: `${p}/guides/context-engineering` },
        { text: prefix ? 'Multimodal Input' : '多模态输入', link: `${p}/guides/multimodal` },
        { text: prefix ? 'Governance & Limits' : '治理与限制', link: `${p}/guides/governance` },
        { text: prefix ? 'Sub-Agents & Handoffs' : 'Sub-Agent 与 Handoff', link: `${p}/guides/sub-agents-and-collaboration` },
        { text: prefix ? 'Workflows' : '工作流', link: `${p}/guides/workflow` },
        { text: prefix ? 'Structured Handoffs & Reducers' : '结构化交接与 Reducer', link: `${p}/guides/structured-output-and-reducers` },
        { text: prefix ? 'Signals & Reactive Agents' : 'Signals 与 Reactive Agent', link: `${p}/guides/signals-and-reactive` },
        { text: prefix ? 'Long-Running Sessions' : '长时间运行 Session', link: `${p}/guides/session-replay-and-recovery` },
        { text: prefix ? 'Evaluation' : '评估', link: `${p}/guides/harness-and-eval` },
        { text: prefix ? 'Phased Acceptance' : '分阶段验收', link: `${p}/guides/milestones` },
        { text: prefix ? 'Runtime Observability' : '运行时观测', link: `${p}/guides/os-profile-and-snapshots` },
      ],
    },
    {
      text: prefix ? 'Tutorial Curriculum' : '教程课程',
      items: [
        { text: 'Research Brief Studio', link: 'https://github.com/kongusen/deepstrike/tree/main/example' },
        { text: prefix ? 'L1: Tools & Sessions' : 'L1：工具与 Session', link: 'https://github.com/kongusen/deepstrike/tree/main/example/01-sourced-qa' },
        { text: prefix ? 'L2: Memory' : 'L2：Memory', link: 'https://github.com/kongusen/deepstrike/tree/main/example/02-memory-assistant' },
        { text: prefix ? 'L3: Skills & Knowledge' : 'L3：Skill 与 Knowledge', link: 'https://github.com/kongusen/deepstrike/tree/main/example/03-skills-handbook' },
        { text: prefix ? 'L4: Signals' : 'L4：Signal', link: 'https://github.com/kongusen/deepstrike/tree/main/example/04-reactive-desk' },
        { text: prefix ? 'L5: Governance' : 'L5：治理', link: 'https://github.com/kongusen/deepstrike/tree/main/example/05-governed-studio' },
        { text: prefix ? 'L6: Long-Running Work' : 'L6：长时间运行', link: 'https://github.com/kongusen/deepstrike/tree/main/example/06-daily-digest' },
        { text: prefix ? 'L7: Specialist Workflow' : 'L7：专业 Agent 工作流', link: 'https://github.com/kongusen/deepstrike/tree/main/example/07-brief-pipeline' },
        { text: prefix ? 'L8: Agent Team' : 'L8：Agent 团队', link: 'https://github.com/kongusen/deepstrike/tree/main/example/08-editorial-room' },
      ],
    },
    {
      text: 'Agent Process Runtime',
      items: [
        { text: prefix ? 'Overview' : '总览', link: `${p}/architecture/` },
        { text: 'Agent Process Runtime', link: `${p}/architecture/agent-process-runtime` },
        { text: prefix ? 'Agent Capabilities Map' : 'Agent 能力图谱', link: `${p}/architecture/diagram-atlas` },
        { text: prefix ? 'Execution Lifecycle' : '执行生命周期', link: `${p}/architecture/execution-model` },
        { text: prefix ? 'Sessions & Recovery' : 'Session 与恢复', link: `${p}/architecture/session-replay` },
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
    {
      text: prefix ? 'Implementation Details' : '实现细节',
      items: [
        { text: prefix ? 'Runtime Implementation Notes' : '运行时实现说明', link: `${p}/architecture/overview` },
        { text: 'Kernel ABI', link: `${p}/architecture/kernel-abi` },
      ],
    },
  ]
}

export const zhSidebar = sidebar('')
export const enSidebar = sidebar('/en')

export const zhNav = [
  { text: '首页', link: '/' },
  { text: 'Agent 能力', link: '/guides/' },
  { text: '快速开始', link: '/getting-started/hello-agent' },
  { text: '教程课程', link: 'https://github.com/kongusen/deepstrike/tree/main/example' },
  { text: 'Agent Runtime', link: '/architecture/agent-process-runtime' },
  { text: '参考', link: '/reference/' },
  { text: 'Wiki', link: 'https://github.com/kongusen/deepstrike/wiki' },
]

export const enNav = [
  { text: 'Home', link: '/en/' },
  { text: 'Agent Capabilities', link: '/en/guides/' },
  { text: 'Quick Start', link: '/en/getting-started/hello-agent' },
  { text: 'Tutorials', link: 'https://github.com/kongusen/deepstrike/tree/main/example' },
  { text: 'Agent Runtime', link: '/en/architecture/agent-process-runtime' },
  { text: 'Reference', link: '/en/reference/' },
  { text: 'Wiki', link: 'https://github.com/kongusen/deepstrike/wiki' },
]
