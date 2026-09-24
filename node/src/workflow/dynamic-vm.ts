import vm from "node:vm"
import type {
  DynamicWorkflowArtifact,
  DynamicWorkflowContext,
  DynamicWorkflowHost,
  DynamicWorkflowRun,
  DynamicWorkflowRunOptions,
  DynamicWorkflowScript,
  DynamicWorkflowVmOptions,
} from "./dynamic.js"
import {
  DynamicWorkflowApprovalError,
  DynamicWorkflowExecutor,
  DynamicWorkflowLimitError,
  DynamicWorkflowReplayMismatchError,
  fingerprintDynamicWorkflowScript,
} from "./dynamic.js"

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
    if (options.trust === "untrusted") {
      throw new DynamicWorkflowScriptError(
        "untrusted dynamic workflows require an OS sandbox executor; node:vm is trusted-only",
      )
    }
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
      const facade = createWorkflowFacade(workflow, context)
      const args = cloneIntoVm(context, workflow.args)
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
      if (error instanceof DynamicWorkflowScriptError || error instanceof DynamicWorkflowApprovalError || error instanceof DynamicWorkflowLimitError || error instanceof DynamicWorkflowReplayMismatchError) throw error
      throw new DynamicWorkflowScriptError(`dynamic workflow "${script.meta.name}" failed`, { cause: error })
    }
  }

  /** Execute a catalogued artifact and bind its digest to replay identity. */
  async runArtifact<TArgs extends Record<string, unknown>, T>(
    artifact: DynamicWorkflowArtifact,
    options: DynamicWorkflowRunOptions<TArgs> = {},
  ): Promise<DynamicWorkflowRun<T>> {
    const expectedDigest = fingerprintDynamicWorkflowScript(artifact.script)
    if (artifact.name !== artifact.script.meta.name || artifact.digest !== expectedDigest) {
      throw new DynamicWorkflowScriptError(`dynamic workflow artifact "${artifact.name}" digest mismatch`)
    }
    return this.runScript<TArgs, T>(artifact.script, {
      ...options,
      artifactDigest: options.artifactDigest ?? artifact.digest,
      artifactSnapshot: options.artifactSnapshot ?? { name: artifact.name, digest: artifact.digest, meta: structuredClone(artifact.script.meta) },
    })
  }
}

function createWorkflowFacade<TArgs extends Record<string, unknown>, T>(workflow: DynamicWorkflowContext<TArgs>, context: vm.Context): DynamicWorkflowContext<TArgs> {
  const facade = Object.create(null) as DynamicWorkflowContext<TArgs>
  Object.defineProperties(facade, {
    args: { enumerable: true, value: cloneIntoVm(context, workflow.args) },
    progress: { enumerable: true, value: cloneIntoVm(context, workflow.progress) },
    agent: { enumerable: true, value: createVmCallable(workflow.agent.bind(workflow)) },
    parallel: { enumerable: true, value: createVmCallable(workflow.parallel.bind(workflow)) },
    parallelAgents: { enumerable: true, value: createVmCallable(workflow.parallelAgents.bind(workflow)) },
    pipeline: { enumerable: true, value: createVmCallable(workflow.pipeline.bind(workflow)) },
    phase: { enumerable: true, value: createVmCallable(workflow.phase.bind(workflow)) },
    log: { enumerable: true, value: createVmCallable(workflow.log.bind(workflow)) },
  })
  return Object.freeze(facade)
}

function createVmCallable<T extends (...args: any[]) => any>(implementation: T): T {
  const callable = (...args: Parameters<T>): ReturnType<T> => implementation(...args)
  Object.setPrototypeOf(callable, null)
  return Object.freeze(callable) as T
}

function cloneIntoVm<T>(context: vm.Context, value: T): T {
  const serialized = JSON.stringify(value)
  return vm.runInContext(serialized === undefined ? "undefined" : `(${serialized})`, context) as T
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
