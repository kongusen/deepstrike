import { contractSurface } from "./contract-surface.mjs"
import { agentFacade } from "./agent-facade.mjs"
import { workflow } from "./workflow.mjs"
import { planesHarnessEvals } from "./planes-harness-evals.mjs"
import { liveSmoke } from "./live-smoke.mjs"

export const SCENARIOS = [contractSurface, agentFacade, workflow, planesHarnessEvals, liveSmoke]
export const SCENARIO_MAP = new Map(SCENARIOS.map(scenario => [scenario.id, scenario]))
