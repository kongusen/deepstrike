/**
 * Real-model MULTIMODAL test: send an image attachment through the framework end-to-end and check the
 * model actually saw it. The framework path: run({ attachments:[ImagePart] }) → attachmentsToKernelMessage
 * → kernel history message (Content::Parts) → render → provider toAnthropicContent → Anthropic image block.
 *
 * We synthesize a distinctive image in-process (top half RED, bottom half BLUE) so a text-only guess can't
 * pass — the model must genuinely see the pixels to name both colors and their positions.
 *
 * Run with:
 *   set -a; source .env; set +a; E2E_PROVIDER=minimax npx jest e2e/multimodal --testTimeout 120000
 */
import { deflateSync } from "node:zlib"
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, collectText } from "../../src/index.js"
import type { ContentPart } from "../../src/types.js"
import { loadProviders, anyProvider } from "./providers.js"

const provider = anyProvider(loadProviders())
const maybe = provider ? describe : describe.skip

// ── minimal PNG encoder (no deps) ─────────────────────────────────────────────
function crc32(buf: Buffer): number {
  let c = ~0
  for (let i = 0; i < buf.length; i++) {
    c ^= buf[i]
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1))
  }
  return ~c >>> 0
}
function chunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length, 0)
  const typeBuf = Buffer.from(type, "ascii")
  const body = Buffer.concat([typeBuf, data])
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(body), 0)
  return Buffer.concat([len, body, crc])
}
/** Solid top-half / bottom-half RGB image as a base64 PNG. */
function twoColorPng(w: number, h: number, top: [number, number, number], bottom: [number, number, number]): string {
  const raw = Buffer.alloc(h * (1 + w * 3))
  for (let y = 0; y < h; y++) {
    const rowStart = y * (1 + w * 3)
    raw[rowStart] = 0 // filter: none
    const [r, g, b] = y < h / 2 ? top : bottom
    for (let x = 0; x < w; x++) {
      const o = rowStart + 1 + x * 3
      raw[o] = r; raw[o + 1] = g; raw[o + 2] = b
    }
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4)
  ihdr[8] = 8; ihdr[9] = 2 // 8-bit, RGB
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ])
  return png.toString("base64")
}

maybe("real-model multimodal (image attachment)", () => {
  it("sees an image sent via run({ attachments }) and names its colors", async () => {
    const b64 = twoColorPng(96, 96, [255, 0, 0], [0, 0, 255]) // red top, blue bottom

    const runner = new RuntimeRunner({
      provider: provider!,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4000,
      maxTurns: 2,
    })

    const attachments: ContentPart[] = [{ type: "image", data: b64, mediaType: "image/png" }]
    const text = (await collectText(
      runner.run({
        sessionId: `mm-${Date.now()}`,
        goal: "Look at the attached image. It is split into two horizontal halves. Name the color of the TOP half and the color of the BOTTOM half. Answer in the form 'top: <color>, bottom: <color>'.",
        attachments,
      }),
    )).toLowerCase()

    console.log(`\n[multimodal] model said: ${text.trim().slice(0, 200)}\n`)

    // Vision check: the model must identify red on top and blue on bottom — unguessable without seeing it.
    const topRed = /top[^a-z]*red/.test(text) || (text.indexOf("red") < text.indexOf("blue") && text.includes("red"))
    expect(text).toContain("red")
    expect(text).toContain("blue")
    expect(topRed).toBe(true)
  }, 120_000)

  // Regression guard for the resume/replay data-loss bug: `run_started.attachments` is persisted but
  // was never read back on resume — reconstruction rebuilt a TEXT-ONLY initial turn, so a crash-and-
  // resume left the model blind to the image. We simulate the crash with a mid-run session log
  // (run_started + attachments, no run_terminal) and resume it; the model must still see the pixels.
  it("preserves an image attachment across a crash-and-resume rebuilt from the session log", async () => {
    const b64 = twoColorPng(96, 96, [255, 0, 0], [0, 0, 255]) // red top, blue bottom
    const sessionLog = new InMemorySessionLog()
    const sessionId = `mm-resume-${Date.now()}`
    const goal =
      "Look at the attached image. It is split into two horizontal halves. Name the color of the TOP half and the color of the BOTTOM half. Answer in the form 'top: <color>, bottom: <color>'."

    // A run that crashed right after starting with an image: the live attachment seed (gated behind
    // !resumeMidRun) never ran, so the only record of the image is the persisted run_started event.
    await sessionLog.append(sessionId, {
      kind: "run_started",
      run_id: "crashed-run",
      goal,
      criteria: [],
      attachments: [{ type: "image", data: b64, mediaType: "image/png" }],
    })

    // Fresh runner, same log → the resume path rebuilds history from events. Before the fix this
    // dropped the image; now it reconstructs a Content::Parts turn and the model can see it again.
    const runner = new RuntimeRunner({
      provider: provider!,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4000,
      maxTurns: 2,
    })
    const text = (await collectText(runner.run({ sessionId, goal }))).toLowerCase()

    console.log(`\n[multimodal-resume] model said: ${text.trim().slice(0, 200)}\n`)

    const topRed = /top[^a-z]*red/.test(text) || (text.indexOf("red") < text.indexOf("blue") && text.includes("red"))
    expect(text).toContain("red")
    expect(text).toContain("blue")
    expect(topRed).toBe(true)
  }, 120_000)
})
