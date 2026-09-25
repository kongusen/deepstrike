import { contractSurface } from "./contract-surface.mjs"
import { agentFacade } from "./agent-facade.mjs"
import { workflow } from "./workflow.mjs"
import { planesHarnessEvals } from "./planes-harness-evals.mjs"
import { liveSmoke } from "./live-smoke.mjs"
import { liveComprehensive } from "./live-comprehensive.mjs"
import { skillProgressive } from "./skill-progressive.mjs"
import { liveSkillProgressive } from "./live-skill-progressive.mjs"

export const SCENARIOS = [contractSurface, agentFacade, workflow, planesHarnessEvals, skillProgressive, liveSmoke, liveComprehensive, liveSkillProgressive]
export const SCENARIO_MAP = new Map(SCENARIOS.map(scenario => [scenario.id, scenario]))
