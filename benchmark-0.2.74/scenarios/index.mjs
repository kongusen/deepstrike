import { contractSurface } from "./contract-surface.mjs"
import { agentFacade } from "./agent-facade.mjs"
import { workflow } from "./workflow.mjs"
import { planesHarnessEvals } from "./planes-harness-evals.mjs"

export const SCENARIOS = [contractSurface, agentFacade, workflow, planesHarnessEvals]
export const SCENARIO_MAP = new Map(SCENARIOS.map(scenario => [scenario.id, scenario]))
