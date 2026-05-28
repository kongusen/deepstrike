/**
 * E2E kernel mechanism test suite.
 *
 * Runs only when at least one provider API key is present in the environment.
 * Skip individual scenarios with E2E_SKIP=K01,K02 or run only specific ones
 * with E2E_ONLY=K06,K07.
 *
 * Usage:
 *   OPENAI_API_KEY=sk-... npm test -- --testPathPattern e2e/run
 *   E2E_ONLY=K03 OPENAI_API_KEY=sk-... npm test -- --testPathPattern e2e/run
 */
import { runScenario, printReport } from "./harness.js"
import { ALL_SCENARIOS } from "./scenarios.js"
import { loadProviders, anyProvider } from "./providers.js"
import type { HarnessResult } from "./harness.js"

// ── env-based filtering ───────────────────────────────────────────────────────

const onlyIds = process.env.E2E_ONLY?.split(",").map(s => s.trim().toUpperCase()) ?? []
const skipIds = process.env.E2E_SKIP?.split(",").map(s => s.trim().toUpperCase()) ?? []

function shouldRun(id: string): boolean {
  if (onlyIds.length > 0) return onlyIds.includes(id.toUpperCase())
  if (skipIds.length > 0) return !skipIds.includes(id.toUpperCase())
  return true
}

// ── provider setup ────────────────────────────────────────────────────────────

const providers = loadProviders()
const defaultProvider = anyProvider(providers)

// If no provider is configured, skip the entire suite
const describeOrSkip = defaultProvider ? describe : describe.skip

// ── test suite ────────────────────────────────────────────────────────────────

describeOrSkip("E2E kernel mechanism tests", () => {
  const results: HarnessResult[] = []

  afterAll(() => printReport(results))

  for (const scenario of ALL_SCENARIOS) {
    const testFn = shouldRun(scenario.id) ? it : it.skip

    testFn(
      `[${scenario.id}] ${scenario.name}`,
      async () => {
        const provider = defaultProvider!
        const result = await runScenario(provider, scenario)
        results.push(result)

        if (!result.passed) {
          // Print detailed metrics on failure before jest swallows them
          console.error(`\n[${result.id}] FAILED: ${result.failure}`)
          console.error(`  turns=${result.turnsUsed}  compressions=${result.compressions}`)
          console.error(`  peak_input_tokens=${result.peakInputTokens}`)
          console.error(`  final_text="${result.finalText.slice(0, 300)}"`)
          console.error("  per-turn metrics:", JSON.stringify(result.metrics.slice(-5), null, 2))
        }

        expect(result.passed).toBe(true)
      },
      // Per-scenario timeout — scenarios can be long
      scenario.timeoutMs ?? 180_000,
    )
  }
})
