/**
 * Provider bootstrap shared by every level.
 *
 * The curriculum runs against a REAL LLM provider (that is the deliberate design choice — the
 * examples exercise the live agent loop, not a scripted transcript). Configure it once via env:
 *
 *   ANTHROPIC_API_KEY=sk-ant-...            → Anthropic (default; set DEEPSTRIKE_MODEL to override)
 *   OPENAI_API_KEY=sk-...                   → OpenAI
 *   DEEPSTRIKE_MODEL=<id> DEEPSTRIKE_API_KEY=...  → any provider via the catalog (createProvider)
 *     (optionally DEEPSTRIKE_PROVIDER / DEEPSTRIKE_BASE_URL for OpenAI-compatible endpoints)
 *
 * `--dry-run` on any level skips this entirely: it validates local wiring without a key or a call.
 */
import { AnthropicProvider, OpenAIProvider, createProvider } from "@deepstrike/sdk"
import type { LLMProvider } from "@deepstrike/sdk"
import { fileURLToPath } from "node:url"
import { dirname, join } from "node:path"

/**
 * Load a `.env` into `process.env` before reading provider config. Tries `example/.env` first,
 * then the repo-root `.env`, so the curriculum picks up an existing root-level key file with no
 * extra setup. Uses Node's built-in loader (20.6+) — no dependency. Missing files are ignored.
 */
export function loadEnv(): void {
  const here = dirname(fileURLToPath(import.meta.url))
  for (const p of [join(here, "..", ".env"), join(here, "..", "..", ".env")]) {
    try {
      process.loadEnvFile(p)
      return
    } catch {
      /* not present — try the next */
    }
  }
}

export function resolveProvider(): LLMProvider {
  const anthropic = process.env.ANTHROPIC_API_KEY
  if (anthropic) {
    return new AnthropicProvider({ apiKey: anthropic, model: process.env.DEEPSTRIKE_MODEL })
  }
  const openai = process.env.OPENAI_API_KEY
  if (openai) {
    // Honor the standard OpenAI env names (what a typical `.env` uses), with DEEPSTRIKE_* as an
    // explicit override. baseURL points an OpenAI-compatible endpoint (proxy / CN vendor).
    return new OpenAIProvider({
      apiKey: openai,
      model: process.env.DEEPSTRIKE_MODEL ?? process.env.OPENAI_MODEL,
      baseURL: process.env.DEEPSTRIKE_BASE_URL ?? process.env.OPENAI_BASE_URL,
    })
  }
  const model = process.env.DEEPSTRIKE_MODEL
  const apiKey = process.env.DEEPSTRIKE_API_KEY
  if (model && apiKey) {
    // The catalog resolves a bare/prefixed/provider-qualified model id to the right vendor client.
    return createProvider({
      model,
      apiKey,
      provider: process.env.DEEPSTRIKE_PROVIDER,
      baseURL: process.env.DEEPSTRIKE_BASE_URL,
    } as Parameters<typeof createProvider>[0])
  }
  throw new Error(
    "No provider configured. Set OPENAI_API_KEY (+OPENAI_BASE_URL/OPENAI_MODEL) or ANTHROPIC_API_KEY.\n" +
      "Or pass --dry-run to validate wiring without a live call.",
  )
}

/** Tiny arg parser shared by the levels: positional goal + `--flag` / `--key value` options. */
export function parseArgs(argv: string[]): { positionals: string[]; flags: Record<string, string | boolean> } {
  const positionals: string[] = []
  const flags: Record<string, string | boolean> = {}
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a.startsWith("--")) {
      const key = a.slice(2)
      const next = argv[i + 1]
      if (next !== undefined && !next.startsWith("--")) {
        flags[key] = next
        i++
      } else {
        flags[key] = true
      }
    } else {
      positionals.push(a)
    }
  }
  return { positionals, flags }
}
