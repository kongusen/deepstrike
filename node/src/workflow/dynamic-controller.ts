import type { WorkflowOutcome, WorkflowSpec } from "../types/agent.js"
import {
  DynamicWorkflowExecutor,
  type DynamicWorkflowContext,
  type DynamicWorkflowHost,
  type DynamicWorkflowRun,
  type DynamicWorkflowRunOptions,
  type DynamicWorkflowProgram,
} from "./dynamic.js"
import { DynamicWorkflowVmExecutor } from "./dynamic-vm.js"

/** A workflow submission waiting for an external kernel driver to execute it. */
export interface DynamicWorkflowSubmission {
  readonly id: string
  readonly spec: WorkflowSpec
}

interface PendingSubmission extends DynamicWorkflowSubmission {
  resolve(outcome: WorkflowOutcome): void
  reject(error: unknown): void
}

/**
 * Bridges a dynamic workflow script to an external kernel driver.
 *
 * The executor owns the dynamic vocabulary, limits, progress, and replay semantics. The controller
 * owns only the asynchronous handoff: a driver consumes `nextSubmission()` and must complete or
 * fail that submission after the kernel has accepted and run it.
 */
export class DynamicWorkflowController<TArgs extends Record<string, unknown> = Record<string, unknown>> {
  private readonly queue: PendingSubmission[] = []
  private readonly active = new Map<string, PendingSubmission>()
  private readonly waiters: Array<(submission: DynamicWorkflowSubmission | undefined) => void> = []
  private nextId = 0
  private finalRun: Promise<DynamicWorkflowRun<unknown>> | undefined
  private finished = false

  start<T>(
    program: DynamicWorkflowProgram<TArgs, T>,
    options: DynamicWorkflowRunOptions<TArgs> = {},
  ): Promise<DynamicWorkflowRun<T>> {
    if (this.finalRun) throw new Error("dynamic workflow controller has already started")
    const host: DynamicWorkflowHost = { runWorkflow: spec => this.enqueue(spec) }
    const run = typeof program === "function"
      ? new DynamicWorkflowExecutor<TArgs>(host, options).run(program)
      : "digest" in program
        ? new DynamicWorkflowVmExecutor(host, options.vmOptions).runArtifact<TArgs, T>(program, options)
        : new DynamicWorkflowVmExecutor(host, options.vmOptions).runScript<TArgs, T>(program, options)
    this.finalRun = run as Promise<DynamicWorkflowRun<unknown>>
    void run.then(() => this.finish(), error => this.finish(error))
    return run
  }

  /** Wait for the next submission, or resolve `undefined` after the script has finished. */
  async nextSubmission(): Promise<DynamicWorkflowSubmission | undefined> {
    const submission = this.queue.shift()
    if (submission) {
      this.active.set(submission.id, submission)
      return submission
    }
    if (this.finished) return undefined
    return new Promise(resolve => this.waiters.push(resolve))
  }

  /** Resolve a submission with the outcome returned by the kernel driver. */
  completeSubmission(id: string, outcome: WorkflowOutcome): void {
    const submission = this.active.get(id)
    if (!submission) throw new Error(`unknown dynamic workflow submission "${id}"`)
    this.active.delete(id)
    submission.resolve(outcome)
  }

  /** Reject a submission when the kernel driver cannot execute it. */
  failSubmission(id: string, error: unknown): void {
    const submission = this.active.get(id)
    if (!submission) throw new Error(`unknown dynamic workflow submission "${id}"`)
    this.active.delete(id)
    submission.reject(error)
  }

  get isFinished(): boolean {
    return this.finished
  }

  private enqueue(spec: WorkflowSpec): Promise<WorkflowOutcome> {
    return new Promise((resolve, reject) => {
      const submission: PendingSubmission = {
        id: `dynamic-submission-${this.nextId++}`,
        spec,
        resolve,
        reject,
      }
      const waiter = this.waiters.shift()
      if (waiter) {
        this.active.set(submission.id, submission)
        waiter(submission)
      } else {
        this.queue.push(submission)
      }
    })
  }

  private finish(error?: unknown): void {
    if (this.finished) return
    this.finished = true
    if (error !== undefined) {
      for (const submission of this.active.values()) submission.reject(error)
      for (const submission of this.queue) submission.reject(error)
    }
    this.active.clear()
    this.queue.length = 0
    for (const waiter of this.waiters.splice(0)) waiter(undefined)
  }
}
