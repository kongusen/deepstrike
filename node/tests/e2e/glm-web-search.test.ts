/**
 * Real-model GLM web_search: confirm the Zhipu endpoint accepts the injected web_search server tool and
 * returns a coherent, search-grounded answer to a question that needs fresh info.
 *
 * Run with:  set -a; source .env; set +a; npx jest e2e/glm-web-search --testTimeout 120000
 * Needs GLM_API_KEY. Uses the z.ai OpenAI-compatible endpoint (the key here is a z.ai key).
 */
import { GLMProvider } from "../../src/providers/glm.js"
import type { RenderedContext } from "../../src/types.js"

const key = process.env.GLM_API_KEY
const maybe = key ? describe : describe.skip

maybe("real-model GLM web_search", () => {
  it("accepts the web_search server tool and answers a current-info question", async () => {
    const provider = new GLMProvider(key!, "glm-5.2", { maxRetries: 2, baseDelay: 800 }, "https://api.z.ai/api/paas/v4")
    const ctx: RenderedContext = {
      systemText: "You answer concisely using up-to-date web information.",
      turns: [{ role: "user", content: "Using web search, what is the latest stable Node.js LTS major version? Reply with just the number." }],
    } as RenderedContext

    const msg = await provider.complete(ctx, [], { web_search: true })
    const text = typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content)
    console.log(`\n[glm web_search] answer: ${text.trim().slice(0, 200)}\n`)

    // Plumbing assertion: the endpoint accepted the web_search tool and returned a non-empty answer.
    expect(text.trim().length).toBeGreaterThan(0)
  }, 120_000)
})
