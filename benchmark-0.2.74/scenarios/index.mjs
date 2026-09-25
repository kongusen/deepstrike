import { contractSurface } from "./contract-surface.mjs"
import { agentFacade } from "./agent-facade.mjs"
import { workflow } from "./workflow.mjs"
import { planesHarnessEvals } from "./planes-harness-evals.mjs"
import { liveSmoke } from "./live-smoke.mjs"
import { liveComprehensive } from "./live-comprehensive.mjs"

export const SCENARIOS = [contractSurface, agentFacade, workflow, planesHarnessEvals, liveSmoke, liveComprehensive]
export const SCENARIO_MAP = new Map(SCENARIOS.map(scenario => [scenario.id, scenario]))
