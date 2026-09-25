export function metric(value, unit, options = {}) {
  return {
    value,
    unit,
    mode: options.mode ?? "deterministic",
    samples: options.samples ?? 1,
  }
}

export function count(value) {
  return metric(value, "count")
}

export function ratio(numerator, denominator) {
  return metric(denominator === 0 ? 0 : numerator / denominator, "ratio")
}

export function flattenNumericMetrics(metrics, prefix = "") {
  const output = {}
  for (const [key, value] of Object.entries(metrics ?? {})) {
    const path = prefix ? `${prefix}.${key}` : key
    if (value && typeof value === "object" && "value" in value && "unit" in value) output[path] = value
    else if (value && typeof value === "object") Object.assign(output, flattenNumericMetrics(value, path))
  }
  return output
}

export function diffMetrics(left, right) {
  const a = flattenNumericMetrics(left)
  const b = flattenNumericMetrics(right)
  const keys = [...new Set([...Object.keys(a), ...Object.keys(b)])].sort()
  return keys.map(key => ({
    key,
    left: a[key]?.value,
    right: b[key]?.value,
    delta: (b[key]?.value ?? 0) - (a[key]?.value ?? 0),
    unit: b[key]?.unit ?? a[key]?.unit,
    changed: (a[key]?.value ?? null) !== (b[key]?.value ?? null),
  }))
}
