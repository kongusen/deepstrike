import { OpenAIProvider, CircuitBreaker } from "@deepstrike/sdk"

export const breaker = new CircuitBreaker(5, 60_000)

export function makeProvider() {
  const key = process.env.OPENAI_API_KEY ?? ""
  const model = process.env.OPENAI_MODEL ?? "gpt-4o-mini"
  const baseURL = process.env.OPENAI_BASE_URL
  return new OpenAIProvider(key, model, { maxRetries: 2, baseDelay: 500 }, baseURL)
}
