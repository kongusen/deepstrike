import vm from "node:vm"
import type {
  DynamicWorkflowContext,
  DynamicWorkflowHost,
  DynamicWorkflowRun,
  DynamicWorkflowRunOptions,
  DynamicWorkflowScript,
} from "./dynamic.js"
import { DynamicWorkflowExecutor } from "./dynamic.js"

export interface DynamicWorkflowVmOptions {
  /** Maximum source bytes accepted from an artifact. */
  maxSourceBytes?: number
  /** Maximum synchronous VM execution time for one script turn. */
  timeoutMs?: number
  /** Maximum wall time for the complete script, including host workflow submissions. */
  maxExecutionMs?: number
}

export class DynamicWorkflowScriptError extends Error {
  readonly code = "DYNAMIC_WORKFLOW_SCRIPT"

  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options)
    this.name = "DynamicWorkflowScriptError"
  }
}

const DEFAULT_VM_OPTIONS: Required<DynamicWorkflowVmOptions> = {
  maxSourceBytes: 256_000,
  timeoutMs: 1_000,
  maxExecutionMs: 30 * 60_000,
}

/** Execute a validated workflow artifact without exposing Node module or process capabilities. */
export class DynamicWorkflowVmExecutor {
  constructor(
    private readonly host: DynamicWorkflowHost,
    private readonly vmOptions: DynamicWorkflowVmOptions = {},
  ) {}

  async runScript<TArgs extends Record<string, unknown>, T>(
    script: DynamicWorkflowScript,
    options: DynamicWorkflowRunOptions<TArgs> = {},
  ): Promise<DynamicWorkflowRun<T>> {
    const limits = { ...DEFAULT_VM_OPTIONS, ...this.vmOptions }
    assertVmOptions(limits)
    assertSafeSource(script.source, limits.maxSourceBytes)
    const context = vm.createContext(Object.create(null), {
      name: `deepstrike-dynamic-workflow:${script.meta.name}`,
      codeGeneration: { strings: false, wasm: false },
    })
    const wrapped = `(async () => { "use strict";\n${script.source}\n})()`
    let vmScript: vm.Script
    try {
      vmScript = new vm.Script(wrapped, {
        filename: `dynamic-workflow:${script.meta.name}`,
      })
    } catch (error) {
      throw new DynamicWorkflowScriptError(`dynamic workflow "${script.meta.name}" could not be compiled`, { cause: error })
    }

    const executor = new DynamicWorkflowExecutor<TArgs>(this.host, options)
    const run = executor.run<T>(workflow => {
      const facade = createWorkflowFacade(workflow)
      const args = workflow.args
      return runWithTimeout(
        () => {
          Object.assign(context, { workflow: facade, args })
          return vmScript.runInContext(context, { timeout: limits.timeoutMs }) as Promise<T>
        },
        limits.maxExecutionMs,
        `dynamic workflow "${script.meta.name}" exceeded maxExecutionMs`,
      )
    })
    try {
      return await run
    } catch (error) {
      if (error instanceof DynamicWorkflowScriptError) throw error
      throw new DynamicWorkflowScriptError(`dynamic workflow "${script.meta.name}" failed`, { cause: error })
    }
  }
}

function createWorkflowFacade<TArgs extends Record<string, unknown>, T>(workflow: DynamicWorkflowContext<TArgs>): DynamicWorkflowContext<TArgs> {
  const facade = Object.create(null) as DynamicWorkflowContext<TArgs>
  Object.defineProperties(facade, {
    args: { enumerable: true, value: workflow.args },
    progress: { enumerable: true, value: workflow.progress },
    agent: { enumerable: true, value: workflow.agent.bind(workflow) },
    parallel: { enumerable: true, value: workflow.parallel.bind(workflow) },
    parallelAgents: { enumerable: true, value: workflow.parallelAgents.bind(workflow) },
    pipeline: { enumerable: true, value: workflow.pipeline.bind(workflow) },
    phase: { enumerable: true, value: workflow.phase.bind(workflow) },
    log: { enumerable: true, value: workflow.log.bind(workflow) },
  })
  return Object.freeze(facade)
}

function assertSafeSource(source: string, maxSourceBytes: number): void {
  if (Buffer.byteLength(source, "utf8") > maxSourceBytes) {
    throw new DynamicWorkflowScriptError(`dynamic workflow source exceeds maxSourceBytes (${maxSourceBytes})`)
  }
  const forbidden = /(?:^|[^\w$])(require|import|export|process|globalThis|global|Buffer|Deno|fetch|WebSocket|child_process|eval|Function)(?:[^\w$]|$)/
  const match = forbidden.exec(source)
  if (match) throw new DynamicWorkflowScriptError(`dynamic workflow source uses forbidden capability "${match[1]}"`)
}

function assertVmOptions(options: Required<DynamicWorkflowVmOptions>): void {
  if (!Number.isInteger(options.maxSourceBytes) || options.maxSourceBytes < 1) throw new DynamicWorkflowScriptError("maxSourceBytes must be a positive integer")
  if (!Number.isInteger(options.timeoutMs) || options.timeoutMs < 1) throw new DynamicWorkflowScriptError("timeoutMs must be a positive integer")
  if (!Number.isInteger(options.maxExecutionMs) || options.maxExecutionMs < 1) throw new DynamicWorkflowScriptError("maxExecutionMs must be a positive integer")
}

async function runWithTimeout<T>(run: () => Promise<T>, timeoutMs: number, message: string): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined
  const deadline = new Promise<never>((_, reject) => {
    timeout = setTimeout(() => reject(new DynamicWorkflowScriptError(message)), timeoutMs)
  })
  try {
    return await Promise.race([run(), deadline])
  } finally {
    if (timeout) clearTimeout(timeout)
  }
}
