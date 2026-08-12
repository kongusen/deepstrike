import { createServer } from "node:http"
import type { AddressInfo } from "node:net"
import { AnthropicProvider } from "../../src/providers/anthropic.js"
import { OpenAIChatProvider } from "../../src/providers/openai.js"
import { OpenAIResponsesProvider } from "../../src/providers/openai-responses.js"
import { GeminiProvider } from "../../src/providers/gemini.js"
import { OllamaProvider } from "../../src/providers/ollama.js"
import { CHARACTERIZATION_CONTEXT as CTX, CHARACTERIZATION_TOOLS as TOOLS } from "./fixtures.js"
import { expectGolden } from "./golden.js"

interface ReceivedRequest {
  method: string
  path: string
  body: unknown
}

function responseFor(path: string): unknown {
  if (path === "/v1/messages") {
    return {
      id: "msg_local",
      type: "message",
      role: "assistant",
      content: [{ type: "text", text: "ok" }],
      model: "claude-opus-4-1",
      stop_reason: "end_turn",
      stop_sequence: null,
      usage: { input_tokens: 1, output_tokens: 1 },
    }
  }
  if (path === "/v1/chat/completions") {
    return {
      id: "chatcmpl_local",
      object: "chat.completion",
      created: 0,
      model: "gpt-4o",
      choices: [{ index: 0, message: { role: "assistant", content: "ok" }, finish_reason: "stop" }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    }
  }
  if (path === "/v1/responses") {
    return {
      id: "resp_local",
      object: "response",
      status: "completed",
      output: [{
        id: "msg_local",
        type: "message",
        status: "completed",
        role: "assistant",
        content: [{ type: "output_text", text: "ok", annotations: [] }],
      }],
      usage: {
        input_tokens: 1,
        input_tokens_details: { cached_tokens: 0 },
        output_tokens: 1,
        output_tokens_details: { reasoning_tokens: 0 },
        total_tokens: 2,
      },
    }
  }
  if (path.includes(":generateContent")) {
    return {
      candidates: [{ content: { role: "model", parts: [{ text: "ok" }] }, finishReason: "STOP", index: 0 }],
      usageMetadata: { promptTokenCount: 1, candidatesTokenCount: 1, totalTokenCount: 2 },
    }
  }
  if (path === "/api/chat") {
    return {
      model: "llama3",
      message: { role: "assistant", content: "ok" },
      done: true,
      prompt_eval_count: 1,
      eval_count: 1,
    }
  }
  return null
}

describe("spc_013-A-00 characterization: exact local endpoint bodies", () => {
  it("sends the characterized complete request through each real SDK transport", async () => {
    const received: ReceivedRequest[] = []
    const server = createServer((req, res) => {
      const chunks: Buffer[] = []
      req.on("data", chunk => chunks.push(Buffer.from(chunk)))
      req.on("end", () => {
        const path = req.url ?? ""
        const raw = Buffer.concat(chunks).toString("utf8")
        received.push({
          method: req.method ?? "",
          path,
          body: raw ? JSON.parse(raw) : null,
        })
        const response = responseFor(path)
        res.statusCode = response === null ? 404 : 200
        res.setHeader("content-type", "application/json")
        res.end(JSON.stringify(response ?? { error: { message: `unexpected path: ${path}` } }))
      })
    })

    await new Promise<void>((resolve, reject) => {
      server.once("error", reject)
      server.listen(0, "127.0.0.1", resolve)
    })
    const { port } = server.address() as AddressInfo
    const root = `http://127.0.0.1:${port}`

    try {
      const providers = [
        new AnthropicProvider({ apiKey: "sk-local", model: "claude-opus-4-1", retry: { maxRetries: 1, baseDelay: 0 }, baseURL: root }),
        new OpenAIChatProvider({ apiKey: "sk-local", model: "gpt-4o", retry: { maxRetries: 1, baseDelay: 0 }, baseURL: `${root}/v1` }),
        new OpenAIResponsesProvider("sk-local", "gpt-4.1", { maxRetries: 1, baseDelay: 0 }, `${root}/v1`),
        new GeminiProvider("sk-local", "gemini-2.0-flash", { maxRetries: 1, baseDelay: 0 }, root),
        new OllamaProvider("llama3", root),
      ]
      for (const provider of providers) {
        const message = await provider.complete(CTX, TOOLS)
        expect(message.content).toBe("ok")
      }
      expectGolden("local-endpoint-bodies", received)
    } finally {
      await new Promise<void>((resolve, reject) => server.close(err => err ? reject(err) : resolve()))
    }
  }, 15_000)
})
