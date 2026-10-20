// Build the current native binding and Node SDK before running this benchmark.
import { performance } from "node:perf_hooks"
import { estimateContextTokens } from "../node/dist/runtime/context-manager.js"

const cases = [
  ["unbroken-ascii-100KiB", "y".repeat(100 * 1024)],
  ["cjk-100KiB", "需求分析与原型设计的详细说明，".repeat(2_300)],
  ["cjk-1MiB", "组件内容与评审意见，".repeat(35_000)],
]
const observations = []
for (const [name, text] of cases) {
  const samples = []
  let tokens = 0
  for (let i = 0; i < 3; i++) {
    const started = performance.now()
    tokens = estimateContextTokens(text)
    samples.push(Math.round((performance.now() - started) * 10) / 10)
  }
  observations.push({ name, utf8Bytes: Buffer.byteLength(text), tokens, milliseconds: samples })
}
process.stdout.write(`${JSON.stringify({
  node: process.version,
  estimator: "chunked-cl100k-with-10-percent-margin",
  observations,
  rssMiB: Math.round(process.memoryUsage().rss / 1024 / 1024),
}, null, 2)}\n`)
