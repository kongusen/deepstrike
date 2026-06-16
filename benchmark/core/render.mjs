/**
 * Render a DiffResult as a fixed-width table.
 *
 * @typedef {import("./diff.mjs").DiffResult} DiffResult
 * @typedef {import("./diff.mjs").DiffRow} DiffRow
 * @typedef {import("./metrics.mjs").MetricValue} MetricValue
 */

/**
 * @param {DiffResult} d
 * @returns {string}
 */
export function renderDiff(d) {
  const header = [
    `scenario ${d.scenarioId}`,
    `baseline=${d.baseline.variantId}(${d.baseline.mode}, n=${d.baseline.samples})`,
    `variant=${d.variant.variantId}(${d.variant.mode}, n=${d.variant.samples})`,
  ].join("  ·  ")

  const lines = []
  const bar = "═".repeat(Math.max(82, header.length + 4))
  lines.push(bar)
  lines.push("  " + header)
  lines.push(bar)
  for (const w of d.warnings) lines.push(`  ⚠  ${w}`)
  if (d.warnings.length) lines.push("")

  const cols = ["metric", "baseline", "variant", "Δ", "Δ%", "sig"]
  const widths = [40, 18, 18, 12, 9, 5]
  lines.push("  " + padRow(cols, widths))
  lines.push("  " + widths.map(w => "─".repeat(w)).join(""))

  let currentLayer = ""
  for (const row of d.rows) {
    if (row.layer !== currentLayer) {
      if (currentLayer) lines.push("")
      lines.push(`  [${row.layer}]`)
      currentLayer = row.layer
    }
    lines.push("  " + padRow(formatRow(row), widths))
  }

  lines.push(bar)
  return lines.join("\n")
}

/** @param {DiffRow} r */
function formatRow(r) {
  return [
    r.key,
    fmtMetric(r.baseline),
    fmtMetric(r.variant),
    r.deltaAbs === null ? "n/a" : fmtNum(r.deltaAbs),
    r.deltaPct === null ? "n/a" : `${r.deltaPct > 0 ? "+" : ""}${r.deltaPct}%`,
    r.significant === null ? "—" : r.significant ? "✓" : " ",
  ]
}

/** @param {MetricValue | null} m */
function fmtMetric(m) {
  if (!m) return "—"
  const base = fmtNum(m.value)
  if (m.stdev !== undefined && (m.samples ?? 1) > 1 && m.stdev > 0) return `${base}±${fmtNum(m.stdev)}`
  return base
}

/** @param {number} n */
function fmtNum(n) {
  if (!isFinite(n)) return String(n)
  if (Math.abs(n) >= 1000) return n.toLocaleString("en-US", { maximumFractionDigits: 0 })
  if (Math.abs(n) >= 1) return String(Math.round(n * 100) / 100)
  if (n === 0) return "0"
  return String(Math.round(n * 10000) / 10000)
}

/**
 * @param {string[]} cells
 * @param {number[]} widths
 */
function padRow(cells, widths) {
  return cells.map((c, i) => pad(c, widths[i])).join("")
}

function pad(s, w) {
  if (s.length >= w) return s.slice(0, w - 1) + " "
  return s + " ".repeat(w - s.length)
}
