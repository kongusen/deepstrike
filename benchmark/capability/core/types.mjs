/**
 * Capability-eval types (JSDoc). Distinct from mechanism BenchScenario A/B.
 *
 * @typedef {Object} CapToolCall
 * @property {string} name
 * @property {Record<string, unknown>} arguments
 *
 * @typedef {Object} CapTask
 * @property {string} id
 * @property {string} goal
 * @property {string} [category]       e.g. bfcl.simple / gaia.l1
 * @property {unknown} [expected]      Adapter-specific ground truth
 * @property {Record<string, unknown>[]} [functions]  BFCL-style function schemas
 * @property {Record<string, unknown>} [meta]         Extra fields (files, level, …)
 *
 * @typedef {Object} CapGrade
 * @property {boolean} passed
 * @property {number} score            0..1
 * @property {string} [reason]
 * @property {Record<string, unknown>} [detail]
 *
 * @typedef {Object} CapResult
 * @property {string} taskId
 * @property {string} sessionId
 * @property {string} status           done / error / exception / timeout
 * @property {string} finalText
 * @property {CapToolCall[]} toolCalls
 * @property {number} wallMs
 * @property {CapGrade} grade
 * @property {string} [error]
 *
 * @typedef {Object} CapReport
 * @property {string} schema           "deepstrike-capability-report/v0"
 * @property {string} suite            bfcl | gaia | webarena
 * @property {string} provider
 * @property {string} model
 * @property {string} startedAt
 * @property {string} finishedAt
 * @property {number} taskCount
 * @property {number} passedCount
 * @property {number} accuracy         passedCount / taskCount
 * @property {number} meanScore
 * @property {CapResult[]} results
 * @property {string} [notes]
 *
 * @typedef {Object} CapAdapter
 * @property {string} id
 * @property {string} description
 * @property {(opts: { limit?: number, dataset?: string }) => Promise<CapTask[]> | CapTask[]} loadTasks
 * @property {(task: CapTask, sdk: any) => Promise<any[]> | any[]} mkTools
 * @property {(args: {
 *   task: CapTask,
 *   finalText: string,
 *   toolCalls: CapToolCall[],
 *   status: string,
 * }) => CapGrade | Promise<CapGrade>} grade
 * @property {string} [systemPrompt]
 * @property {number} [maxTurns]
 * @property {number} [maxTokens]
 * @property {number} [timeoutMs]
 */

export {}
