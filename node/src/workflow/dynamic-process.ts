import { spawn } from "node:child_process"
import { createInterface } from "node:readline"
import type {
  DynamicWorkflowArtifact,
  DynamicWorkflowContext,
  DynamicWorkflowHost,
  DynamicWorkflowRun,
  DynamicWorkflowRunOptions,
  DynamicWorkflowScript,
  DynamicWorkflowAgentRequest,
} from "./dynamic.js"
import {
  DynamicWorkflowExecutor,
  fingerprintDynamicWorkflowScript,
} from "./dynamic.js"
import { DynamicWorkflowScriptError } from "./dynamic-vm.js"

export interface DynamicWorkflowProcessOptions {
  /** Directory exposed as the child process cwd. The process still has no host bindings. */
  cwd?: string
  /** Environment passed to the child; defaults to a minimal PATH/HOME/TMPDIR set. */
  env?: Record<string, string>
}

interface RpcRequest { type: "request"; id: number; method: string; payload?: unknown }
interface RpcResponse { type: "response"; id: number; ok: boolean; value?: unknown; error?: string }
type WorkerResult = { type: "result"; value?: unknown } | { type: "error"; error: string }

/**
 * Runs an untrusted dynamic artifact in a killable child process. The parent owns all workflow
 * semantics; the child only evaluates source and asks the parent for typed workflow operations.
 */
export class DynamicWorkflowProcessExecutor {
  constructor(
    private readonly host: DynamicWorkflowHost,
    private readonly processOptions: DynamicWorkflowProcessOptions = {},
  ) {}

  async runScript<TArgs extends Record<string, unknown>, T>(
    script: DynamicWorkflowScript,
    options: DynamicWorkflowRunOptions<TArgs> = {},
  ): Promise<DynamicWorkflowRun<T>> {
    const executor = new DynamicWorkflowExecutor(this.host, options)
    return executor.run<T>(ctx => this.runInProcess(script, ctx, options))
  }

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
      trust: "untrusted",
      artifactDigest: options.artifactDigest ?? artifact.digest,
      artifactSnapshot: options.artifactSnapshot ?? { name: artifact.name, digest: artifact.digest, meta: structuredClone(artifact.script.meta) },
    })
  }

  private runInProcess<TArgs extends Record<string, unknown>, T>(
    script: DynamicWorkflowScript,
    context: DynamicWorkflowContext<TArgs>,
    options: DynamicWorkflowRunOptions<TArgs>,
  ): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const child = spawn(process.execPath, ["--input-type=module", "-e", WORKER_SOURCE], {
        cwd: this.processOptions.cwd,
        env: this.processOptions.env ?? { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: "/tmp", TMPDIR: "/tmp" },
        stdio: ["pipe", "pipe", "pipe"],
      })
    const phases = new Map<string, { resolve(): void; reject(error: Error): void }>()
      const lines = createInterface({ input: child.stdout })
      let nextId = 0
      let settled = false
      let stderr = ""
      const maxExecutionMs = options.vmOptions?.maxExecutionMs ?? 30 * 60_000
      const timeout = setTimeout(() => {
        child.kill("SIGKILL")
        finishError(new DynamicWorkflowScriptError(`dynamic workflow "${script.meta.name}" exceeded maxExecutionMs`))
      }, maxExecutionMs)

      const send = (message: Record<string, unknown>): void => {
        if (!child.stdin.destroyed) child.stdin.write(`${JSON.stringify(message)}\n`)
      }
      const respond = (request: RpcRequest, value?: unknown, error?: unknown): void => {
        send({ type: "response", id: request.id, ok: error === undefined, ...(error === undefined ? { value } : { error: error instanceof Error ? error.message : String(error) }) })
      }
      const finishError = (error: unknown): void => {
        if (settled) return
        settled = true
        clearTimeout(timeout)
        lines.close()
        reject(error instanceof Error ? error : new Error(String(error)))
        child.kill("SIGKILL")
      }
      const finishValue = (value: unknown): void => {
        if (settled) return
        settled = true
        clearTimeout(timeout)
        lines.close()
        resolve(value as T)
        child.kill()
      }
      const handleRequest = async (request: RpcRequest): Promise<void> => {
        try {
          const payload = (request.payload ?? {}) as Record<string, unknown>
          if (request.method === "agent") {
            respond(request, await context.agent(String(payload.prompt ?? ""), payload.options as never))
            return
          }
          if (request.method === "parallelAgents") {
            const requests = (payload.requests ?? []) as DynamicWorkflowAgentRequest[]
            respond(request, await context.parallelAgents(requests, (_, index) => requests[index]))
            return
          }
          if (request.method === "log") {
            context.log(String(payload.message ?? ""), payload.fields as Record<string, unknown> | undefined)
            respond(request, true)
            return
          }
          if (request.method === "progress") {
            respond(request, context.progress)
            return
          }
          if (request.method === "phase") {
            const phaseId = String(payload.phaseId ?? request.id)
            const completion = new Promise<void>((resolvePhase, rejectPhase) => phases.set(phaseId, { resolve: resolvePhase, reject: rejectPhase }))
            void context.phase(String(payload.name ?? ""), async () => {
              respond(request, true)
              await completion
            }).catch(error => { phases.delete(phaseId); respond(request, undefined, error) })
            return
          }
          if (request.method === "phase_end") {
            const phaseId = String(payload.phaseId ?? "")
            const phase = phases.get(phaseId)
            if (!phase) throw new Error(`unknown dynamic workflow phase "${phaseId}"`)
            phases.delete(phaseId)
            if (payload.error) phase.reject(new Error(String(payload.error)))
            else phase.resolve()
            respond(request, true)
            return
          }
          throw new Error(`unsupported dynamic workflow process request "${request.method}"`)
        } catch (error) {
          respond(request, undefined, error)
        }
      }
      lines.on("line", line => {
        let message: RpcRequest | RpcResponse | WorkerResult | undefined
        try { message = JSON.parse(line) as RpcRequest | RpcResponse | WorkerResult } catch { finishError(new DynamicWorkflowScriptError("dynamic workflow process emitted invalid protocol data")); return }
        if (message.type === "request") { void handleRequest(message as RpcRequest); return }
        if (message.type === "result") { finishValue(message.value); return }
        if (message.type === "error") { finishError(new DynamicWorkflowScriptError(message.error)); return }
      })
      child.stderr.on("data", chunk => { stderr += String(chunk).slice(0, 4_096) })
      child.on("error", finishError)
      child.on("close", code => { if (!settled && code !== 0) finishError(new DynamicWorkflowScriptError(`dynamic workflow process exited with code ${code}${stderr ? `: ${stderr.trim()}` : ""}`)) })
      const signal = options.signal
      const abort = () => { child.kill("SIGTERM"); finishError(new DynamicWorkflowScriptError("dynamic workflow process was cancelled")) }
      signal?.addEventListener("abort", abort, { once: true })
      const cleanup = () => signal?.removeEventListener("abort", abort)
      send({ type: "init", script, args: context.args, timeoutMs: options.vmOptions?.timeoutMs ?? 1_000 })
      // Cleanup is tied to settlement without changing the Promise executor's public shape.
      const settleCheck = setInterval(() => { if (settled) { clearInterval(settleCheck); cleanup() } }, 10)
    })
  }
}

const WORKER_SOURCE = String.raw`
import vm from "node:vm";
import readline from "node:readline";
let nextId = 0;
const pending = new Map();
const send = value => process.stdout.write(JSON.stringify(value) + "\n");
const rpc = (method, payload) => new Promise((resolve, reject) => { const id = ++nextId; pending.set(id, { resolve, reject }); send({ type: "request", id, method, payload }); });
const input = readline.createInterface({ input: process.stdin });
input.on("line", async line => {
  const message = JSON.parse(line);
  if (message.type === "response") { const waiter = pending.get(message.id); if (!waiter) return; pending.delete(message.id); message.ok ? waiter.resolve(message.value) : waiter.reject(new Error(message.error)); return; }
  if (message.type !== "init") return;
  try {
    const max = message.timeoutMs;
    const phase = async (name, body) => { const phaseId = "phase-" + (++nextId); await rpc("phase", { name, phaseId }); let failure; try { return await body(); } catch (error) { failure = String(error?.message ?? error); throw error; } finally { await rpc("phase_end", { phaseId, ...(failure ? { error: failure } : {}) }); } };
    const workflow = Object.freeze(Object.assign(Object.create(null), {
      args: Object.freeze(message.args ?? {}),
      progress: Object.freeze(await rpc("progress")),
      agent: (prompt, options) => rpc("agent", { prompt, options }),
      parallelAgents: async (items, worker) => rpc("parallelAgents", { requests: items.map((item, index) => worker(item, index)) }),
      parallel: async (items, worker) => Promise.all(items.map((item, index) => worker(item, index))),
      pipeline: async (items, worker) => { const out = []; for (let index = 0; index < items.length; index++) out.push(await worker(items[index], index)); return out; },
      phase,
      log: (message, fields) => rpc("log", { message, fields }),
    }));
    const source = message.script.source;
    if (/(?:^|[^\w$])(require|import|export|process|globalThis|global|Buffer|Deno|fetch|WebSocket|child_process|eval|Function|Date|performance|Math\.random)(?:[^\w$]|$)/.test(source)) throw new Error("dynamic workflow source uses forbidden capability");
    const context = vm.createContext(Object.create(null), { codeGeneration: { strings: false, wasm: false } });
    Object.assign(context, { workflow, args: Object.freeze(message.args ?? {}) });
    const result = await new vm.Script("(async () => {\"use strict\";\n" + source + "\n})()", { filename: "dynamic-workflow-process" }).runInContext(context, { timeout: max });
    send({ type: "result", value: result });
  } catch (error) { send({ type: "error", error: String(error?.message ?? error) }); }
});
`;
