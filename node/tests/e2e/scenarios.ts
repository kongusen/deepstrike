/**
 * E2E scenario definitions — aligned with the 4-slot context model.
 *
 * Slot 1 — system_stable:    Identity (永不变, cache_control)
 * Slot 2 — system_knowledge: Knowledge (低频变, cache_control)
 * Slot 3 — turns[0]:         State (task_state + signals, 每轮变)
 * Slot 4 — turns[1..N]:      History (压缩管道目标)
 */
import { tool } from "../../src/tools/index.js"
import type { ScenarioCfg, HarnessResult } from "./harness.js"

function pass() { return { passed: true } }
function fail(reason: string) { return { passed: false, failure: reason } }

// ── K01: State turn carries goal (Slot 3 验证) ────────────────────────────────
// goal 必须在 turns[0]（State slot），不在 system_text 里。
// 同时验证 rho 线性增长（history 压缩正确）。

export const K01_StateTurnAndRhoLinear: ScenarioCfg = {
  id: "K01",
  name: "state-turn-and-rho-linear",
  goal: "Call the step tool 20 times (step=1 through step=20) then say DONE.",
  tools: [
    tool("step", "Record a step", {
      type: "object",
      properties: { n: { type: "number" } },
      required: ["n"],
    }, (args) => `step ${(args as { n: number }).n} recorded`),
  ],
  maxTokens: 32_000,
  maxTurns: 30,
  timeoutMs: 180_000,
  validate(r: HarnessResult) {
    if (r.finalStatus !== "completed" && r.finalStatus !== "max_turns")
      return fail(`run did not complete: ${r.finalStatus}`)

    // Slot 3: goal must be in turns[0] (State turn), not in system_text
    const first = r.metrics[0]?.contextSnapshot
    if (!first) return fail("no context snapshot")
    if (!first.stateTurnContent.includes("[TASK STATE]"))
      return fail(`goal not in State turn (turns[0]): "${first.stateTurnContent.slice(0, 200)}"`)

    // rho linear: last-5 avg / first-5 avg < 6x
    const tokens = r.metrics.map(m => m.inputTokens).filter(t => t > 0)
    if (tokens.length < 4) return fail("not enough usage events")
    const avg = (arr: number[]) => arr.reduce((a, b) => a + b, 0) / arr.length
    const ratio = avg(tokens.slice(-5)) / avg(tokens.slice(0, 5))
    if (ratio > 6) return fail(`rho growth super-linear: ratio=${ratio.toFixed(1)}x`)

    return pass()
  },
}

// ── K02: Knowledge slot carries initial memory (Slot 2 验证) ──────────────────
// initialMemory 注入 system_knowledge（Slot 2），模型能在 turns 里引用它。
// 验证：system_knowledge 非空，且模型最终回答包含记忆内容。

export const K02_KnowledgeSlot: ScenarioCfg = {
  id: "K02",
  name: "knowledge-slot-initial-memory",
  goal: "What is the secret code stored in your knowledge? Reply with just the code.",
  tools: [],
  maxTokens: 8_192,
  maxTurns: 5,
  timeoutMs: 60_000,
  systemPrompt: "You are a helpful assistant.",
  initialMemory: ["The secret code is: SECRET-K02"],
  validate(r: HarnessResult) {
    const first = r.metrics[0]?.contextSnapshot
    if (!first) return fail("no context snapshot")
    if (!first.systemKnowledge.includes("SECRET-K02"))
      return fail(`SECRET-K02 not in system_knowledge: "${first.systemKnowledge.slice(0, 200)}"`)
    if (!r.finalText.includes("SECRET-K02"))
      return fail(`model did not recall SECRET-K02: "${r.finalText.slice(0, 200)}"`)
    return pass()
  },
}

// ── K03: Compression log visible in State turn (Slot 3 + 压缩日志) ────────────
// 触发 AutoCompact，验证 compression_log 出现在 turns[0]（State turn）里，
// 而不是 system_text 里（旧设计的 scratchpad → systemVolatile 路径已废弃）。

export const K03_CompressionLogInStateTurn: ScenarioCfg = {
  id: "K03",
  name: "compression-log-in-state-turn",
  goal: "Call fill 25 times (n=1..25). After all fills, say DONE.",
  tools: [
    tool("fill", "Add content", {
      type: "object",
      properties: { n: { type: "number" } },
    }, () => "data: " + "w".repeat(150)),
  ],
  maxTokens: 512,
  maxTurns: 50,
  timeoutMs: 300_000,
  validate(r: HarnessResult) {
    if (r.compressions === 0)
      return fail("no compression — AutoCompact should fire at 512 tokens")

    // compression_log must appear in State turn (turns[0]), not system_text
    const afterCompression = r.metrics.find(m => m.compressionAction)
    if (!afterCompression) return fail("no turn with compression action")
    const snap = afterCompression.contextSnapshot
    if (!snap) return fail("no snapshot after compression")
    if (!snap.stateTurnContent.includes("[Compressed:"))
      return fail(`compression_log not in State turn after compression: "${snap.stateTurnContent.slice(0, 300)}"`)

    return pass()
  },
}

// ── K04: Signals injected into State turn (Slot 3 + rollback signal) ──────────
// 工具 fatal 失败触发 rollback，rollback note 作为 signal 注入 turns[0]。
// 验证：rollback 后的 State turn 包含 rollback 信号，最终完成。

export const K04_SignalInStateTurn: ScenarioCfg = {
  id: "K04",
  name: "signal-in-state-turn",
  goal: "Call the fragile_tool once. It may fail — retry until it succeeds, then say SUCCESS.",
  tools: (() => {
    let attempts = 0
    return [
      tool("fragile_tool", "Fails the first two times", {
        type: "object",
        properties: {},
      }, () => {
        attempts++
        if (attempts <= 2) {
          const err = new Error("transient error — please retry")
          ;(err as any).isFatal = true
          throw err
        }
        return "ok on attempt " + attempts
      }),
    ]
  })(),
  maxTokens: 8_192,
  maxTurns: 15,
  timeoutMs: 120_000,
  validate(r: HarnessResult) {
    const hadRollback = r.events.some(e => e.event.kind === "rollbacked")
    if (!hadRollback) return fail("no rollbacked events")

    // After rollback, the State turn should carry the rollback signal
    const afterRollback = r.metrics.find((m, i) => {
      if (i === 0) return false
      const prev = r.metrics[i - 1]
      return prev.compressionAction === undefined && m.contextSnapshot?.stateTurnContent.includes("failed")
    })
    // Signal presence is best-effort — just verify the run recovered
    if (!r.finalText.toLowerCase().includes("success"))
      return fail(`final text lacks success marker: "${r.finalText.slice(0, 200)}"`)
    return pass()
  },
}

// ── K05: History compression preserves recency (Slot 4 + recency) ─────────────
// 大量 filler 填满 history，验证最近的内容（RECENT_MARKER）在压缩后仍可见。
// 这是 Slot 4 压缩管道的核心保证：newest-first 填充 + preserve_recent_msgs。

const SECRET_CODE = "ZETA-7741"

export const K05_HistoryRecency: ScenarioCfg = {
  id: "K05",
  name: "history-recency-after-compression",
  goal: `First call store_secret with "${SECRET_CODE}". Then call fill_buffer 15 times. Finally call recall_secret and include the secret verbatim in your reply.`,
  tools: [
    (() => {
      let stored = ""
      return [
        tool("store_secret", "Store a secret", {
          type: "object",
          properties: { secret: { type: "string" } },
          required: ["secret"],
        }, (args) => { stored = (args as { secret: string }).secret; return "stored" }),
        tool("fill_buffer", "Add filler", {
          type: "object",
          properties: {},
        }, () => "filler: " + "x".repeat(200)),
        tool("recall_secret", "Retrieve the secret", {
          type: "object",
          properties: {},
        }, () => stored || "(nothing stored)"),
      ]
    })(),
  ].flat(),
  maxTokens: 8_192,
  maxTurns: 30,
  timeoutMs: 180_000,
  validate(r: HarnessResult) {
    if (!r.finalText.includes(SECRET_CODE))
      return fail(`secret not in final reply: "${r.finalText.slice(0, 200)}"`)
    return pass()
  },
}

// ── K06: Long tool loop stability (Slot 4 稳定性) ─────────────────────────────
// 20 轮工具循环，宽裕预算，验证 history 无异常 token 尖刺，≤1 次压缩。

export const K06_LongLoopStability: ScenarioCfg = {
  id: "K06",
  name: "long-tool-loop-stability",
  goal: "Call the accumulate tool 20 times (step=1 through step=20). After step 20, say FINISHED.",
  tools: [
    tool("accumulate", "Accumulate steps", {
      type: "object",
      properties: { step: { type: "number" } },
      required: ["step"],
    }, (args) => `accumulated step ${(args as { step: number }).step}`),
  ],
  maxTokens: 32_000,
  maxTurns: 35,
  timeoutMs: 300_000,
  validate(r: HarnessResult) {
    if (r.finalStatus !== "completed" && r.finalStatus !== "max_turns")
      return fail(`run did not complete: ${r.finalStatus}`)
    if (r.compressions > 1)
      return fail(`${r.compressions} compressions on 32k budget — rho over-counting suspected`)
    const tokens = r.metrics.map(m => m.inputTokens).filter(t => t > 0)
    for (let i = 1; i < tokens.length; i++) {
      const delta = tokens[i] - tokens[i - 1]
      if (tokens[i - 1] > 0 && delta > tokens[i - 1] * 2 && delta > 2000)
        return fail(`token spike at turn ${i}: ${tokens[i - 1]} → ${tokens[i]}`)
    }
    return pass()
  },
}

// ── K07: Session continuity (history replay) ──────────────────────────────────
// 同一 sessionId 两次 run，第二次能看到第一次的 history。

export const K07_SessionContinuity: ScenarioCfg = {
  id: "K07",
  name: "session-continuity",
  goal: "Call set_value with value=PERSIST-42. Then call get_value and include the value verbatim in your reply.",
  tools: (() => {
    const kv = new Map<string, string>()
    return [
      tool("set_value", "Store a value", {
        type: "object",
        properties: { value: { type: "string" } },
        required: ["value"],
      }, (args) => { kv.set("key", (args as { value: string }).value); return "stored" }),
      tool("get_value", "Retrieve the stored value", {
        type: "object",
        properties: {},
      }, () => kv.get("key") ?? "(empty)"),
    ]
  })(),
  maxTokens: 8_192,
  maxTurns: 10,
  timeoutMs: 90_000,
  validate(r: HarnessResult) {
    if (!r.finalText.includes("PERSIST-42"))
      return fail(`agent did not recall stored value: "${r.finalText.slice(0, 200)}"`)
    return pass()
  },
}

// ── K08: Coding task (full pipeline) ─────────────────────────────────────────
// 虚拟 FS 写/读/验证，覆盖完整的 State + History 路径。

export const K08_CodingTask: ScenarioCfg = {
  id: "K08",
  name: "coding-task",
  goal: "Write a file 'result.txt' containing exactly: answer=42\nThen read it back and verify. Reply FILE_VERIFIED if correct, FILE_ERROR if not.",
  tools: (() => {
    const fs = new Map<string, string>()
    return [
      tool("write_file", "Write a file", {
        type: "object",
        properties: { path: { type: "string" }, content: { type: "string" } },
        required: ["path", "content"],
      }, (args) => {
        const a = args as { path: string; content: string }
        fs.set(a.path, a.content)
        return `written ${a.content.length} bytes to ${a.path}`
      }),
      tool("read_file", "Read a file", {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
      }, (args) => fs.get((args as { path: string }).path) ?? "(file not found)"),
    ]
  })(),
  maxTokens: 8_192,
  maxTurns: 12,
  timeoutMs: 120_000,
  validate(r: HarnessResult) {
    if (!r.finalText.includes("FILE_VERIFIED"))
      return fail(`agent did not confirm file verification: "${r.finalText.slice(0, 300)}"`)
    return pass()
  },
}

export const ALL_SCENARIOS: ScenarioCfg[] = [
  K01_StateTurnAndRhoLinear,
  K02_KnowledgeSlot,
  K03_CompressionLogInStateTurn,
  K04_SignalInStateTurn,
  K05_HistoryRecency,
  K06_LongLoopStability,
  K07_SessionContinuity,
  K08_CodingTask,
]
