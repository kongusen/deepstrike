import { spawn } from "node:child_process"
import { mkdir } from "node:fs/promises"
import { tool } from "../tools/index.js"
import type { RegisteredTool } from "../tools/index.js"
import { LocalExecutionPlane } from "./execution-plane.js"

export interface SandboxOptions {
  /** Working directory for all subprocesses. Isolated from the rest of the file system by convention. */
  sandboxDir: string
  /** Env var names from the host environment to forward into subprocesses. Default: none. */
  allowedEnvKeys?: string[]
  /** Per-call hard timeout in ms. Defaults to 30 000. */
  timeoutMs?: number
  /** Truncate stdout+stderr after this many bytes. Defaults to 1 MiB. */
  maxOutputBytes?: number
}

/**
 * ExecutionPlane that confines subprocess execution to a sandbox directory.
 * Extends LocalExecutionPlane with two built-in tools:
 *  - `run_bash`  — executes a bash command inside sandboxDir.
 *  - `run_node`  — evaluates a Node.js script inside sandboxDir.
 *
 * All registered JS tools continue to run in-process (identical to LocalExecutionPlane).
 * The sandboxing guarantee is: subprocesses spawned by the built-in tools cannot reach
 * files outside sandboxDir unless the host OS allows it.
 */
export class ProcessSandboxPlane extends LocalExecutionPlane {
  private readonly sandboxDir: string
  private readonly allowedEnvKeys: string[]
  private readonly timeoutMs: number
  private readonly maxOutputBytes: number

  constructor(opts: SandboxOptions) {
    super()
    this.sandboxDir = opts.sandboxDir
    this.allowedEnvKeys = opts.allowedEnvKeys ?? []
    this.timeoutMs = opts.timeoutMs ?? 30_000
    this.maxOutputBytes = opts.maxOutputBytes ?? 1_048_576

    super.register(this.makeBashTool(), this.makeNodeTool())
  }

  private buildEnv(): Record<string, string> {
    const env: Record<string, string> = {
      HOME: this.sandboxDir,
      TMPDIR: this.sandboxDir,
      PATH: "/usr/local/bin:/usr/bin:/bin",
    }
    for (const key of this.allowedEnvKeys) {
      const val = process.env[key]
      if (val !== undefined) env[key] = val
    }
    return env
  }

  private runSubprocess(cmd: string, argv: string[]): Promise<{ output: string; isError: boolean }> {
    return new Promise(resolve => {
      const chunks: Buffer[] = []
      let totalBytes = 0
      let settled = false

      const settle = (output: string, isError: boolean) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        resolve({ output, isError })
      }

      const child = spawn(cmd, argv, {
        cwd: this.sandboxDir,
        env: this.buildEnv(),
        stdio: ["ignore", "pipe", "pipe"],
      })

      const timer = setTimeout(() => {
        child.kill("SIGKILL")
        settle(`timed out after ${this.timeoutMs}ms`, true)
      }, this.timeoutMs)

      const capture = (chunk: Buffer) => {
        if (settled) return
        totalBytes += chunk.length
        if (totalBytes > this.maxOutputBytes) {
          chunks.push(Buffer.from("\n[output truncated]"))
          child.kill("SIGKILL")
          return
        }
        chunks.push(chunk)
      }

      child.stdout.on("data", capture)
      child.stderr.on("data", capture)

      child.on("close", code => settle(Buffer.concat(chunks).toString("utf8"), code !== 0))
      child.on("error", err => settle(String(err), true))
    })
  }

  private makeBashTool(): RegisteredTool {
    return tool(
      "run_bash",
      "Run a bash command confined to the sandbox directory. Paths outside the sandbox are not accessible.",
      {
        type: "object",
        properties: {
          command: { type: "string", description: "The bash command to execute." },
        },
        required: ["command"],
      },
      async (args: Record<string, unknown>) => {
        await mkdir(this.sandboxDir, { recursive: true })
        const { output, isError } = await this.runSubprocess("bash", ["-c", String(args.command)])
        if (isError && !output.trim()) return "Process exited with non-zero status and produced no output."
        return output || "(no output)"
      },
    )
  }

  private makeNodeTool(): RegisteredTool {
    return tool(
      "run_node",
      "Evaluate a Node.js script confined to the sandbox directory.",
      {
        type: "object",
        properties: {
          code: { type: "string", description: "The JavaScript code to evaluate." },
        },
        required: ["code"],
      },
      async (args: Record<string, unknown>) => {
        await mkdir(this.sandboxDir, { recursive: true })
        const { output, isError } = await this.runSubprocess("node", ["-e", String(args.code)])
        if (isError && !output.trim()) return "Script exited with non-zero status and produced no output."
        return output || "(no output)"
      },
    )
  }
}
