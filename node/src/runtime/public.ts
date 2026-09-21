/** Runtime subpath: host execution primitives intentionally outside the root quick-start API. */
export { RuntimeRunner, collectText } from "./runner.js"
export type { RuntimeOptions } from "./runner.js"
export { runAgent, runFanout } from "./facade.js"
export type { RunAgentOptions, RunFanoutOptions } from "./facade.js"
export { InMemorySessionLog, FileSessionLog } from "./session-log.js"
export type { SessionLog, SessionEvent, SessionEventKind } from "./session-log.js"
export { projectAgentRun, projectAgentContext, projectAgentCapabilities, projectAgentGovernance, projectAgentDelegation } from "../agent-ir.js"
export type { AgentDescriptor } from "../agent-ir.js"
