import assert from "node:assert/strict"
import test from "node:test"
import {
  ProviderError,
  classifyProviderError,
  modelRegistry,
  openAIChatDialects,
} from "@deepstrike/sdk/providers"

test("published providers subpath exposes the SPC-013 runtime ABI", () => {
  const cause = Object.assign(new Error("slow down"), { status: 429 })
  const error: ProviderError = classifyProviderError("openai", cause)
  assert.equal(error.kind, "rate_limit")
  assert.equal(error.cause, cause)
  assert.ok(Object.keys(modelRegistry).length > 0)
  assert.equal(openAIChatDialects.openai.providerId, "openai")
})
