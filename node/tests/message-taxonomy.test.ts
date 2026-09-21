import type { ModelMessage, ProviderMessage, RuntimeMessage, StoredMessage, WireMessage } from "../src/types.js"
import { PROJECTION_PAIRS } from "../src/projection-pairs.js"

test("SPC-028-07 ProviderMessage is a compatibility mirror of ModelMessage", () => {
  const message: ModelMessage = { role: "assistant", content: "ready" }
  const provider: ProviderMessage = message
  const stored: StoredMessage = { ...provider, messageId: "m1", createdAt: 1 }
  const runtime: RuntimeMessage = { ...stored }
  const wire: WireMessage = { role: runtime.role, content: runtime.content }
  expect(wire).toEqual({ role: "assistant", content: "ready" })
})

test("SPC-028-08 and 09 register content authority and projection directions", () => {
  expect(PROJECTION_PAIRS.Content.authority).toBe("ContentPart[]")
  expect(PROJECTION_PAIRS.ToolResult.authority).toBe("contentParts")
  for (const pair of Object.values(PROJECTION_PAIRS)) {
    expect(pair.authority).toEqual(expect.any(String))
    expect(pair.projection).toEqual(expect.any(String))
    expect(pair.crossingFunction).toEqual(expect.any(String))
  }
})
