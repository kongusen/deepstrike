// Golden master for the Anthropic-compatible vendor providers (P1 refactor §6.1).
// Locks descriptor() + runtimePolicy() across every known model so the
// data-driven `AnthropicCompatibleProvider` refactor stays byte-for-byte
// behavior-preserving. Values transcribed from the pre-refactor vendor classes.
import { DeepSeekAnthropicProvider } from "../src/providers/deepseek.js"
import { KimiAnthropicProvider } from "../src/providers/kimi.js"
import { QwenAnthropicProvider } from "../src/providers/qwen.js"
import { GLMAnthropicProvider } from "../src/providers/glm.js"
import { MiniMaxAnthropicProvider } from "../src/providers/minimax.js"
import type { LLMProvider } from "../src/types.js"

type Ctor = new (apiKey: string, model?: string, retry?: { maxRetries: number; baseDelay: number }, baseURL?: string) => LLMProvider

interface Case {
  name: string
  Ctor: Ctor
  defaultModel: string
  policies: Record<string, number>
}

const baseDescriptor = (provider: string, model: string) => ({
  provider,
  protocol: "anthropic-messages",
  model,
  reasoning: { supported: true, preserveAcrossToolTurns: true, requiresReplayForToolTurns: true },
  toolCalls: { supported: true, requiresStrictPairing: true },
})

const CASES: Case[] = [
  {
    name: "deepseek", Ctor: DeepSeekAnthropicProvider, defaultModel: "deepseek-v4-flash",
    policies: { "deepseek-chat": 25, "deepseek-reasoner": 50, "deepseek-v4-flash": 20, "deepseek-v4-pro": 35 },
  },
  {
    name: "kimi", Ctor: KimiAnthropicProvider, defaultModel: "kimi-k2.6",
    policies: {
      "moonshot-v1-8k": 15, "moonshot-v1-32k": 20, "moonshot-v1-128k": 30,
      "kimi-k2.5": 30, "kimi-k2.6": 35, "kimi-k2-thinking": 50, "kimi-k2-thinking-turbo": 40,
    },
  },
  {
    name: "qwen", Ctor: QwenAnthropicProvider, defaultModel: "qwen3.6-plus",
    policies: {
      "qwen3.7-max-preview": 45, "qwen3.7-plus-preview": 40, "qwen3.6-max-preview": 40,
      "qwen3.6-plus": 35, "qwen3.6-flash": 20, "qwen3.6-35b-a3b": 25, "qwen3.6-27b": 25,
      "qwen3.5-plus": 35, "qwen3.5-flash": 20, "qwen3.5-397b-a17b": 35,
      "qwen3.5-122b-a10b": 25, "qwen3.5-35b-a3b": 20, "qwen3.5-27b": 20,
    },
  },
  {
    name: "glm", Ctor: GLMAnthropicProvider, defaultModel: "glm-5.1",
    policies: {
      "glm-5.1": 50, "glm/glm-5.1": 50, "glm-4-plus": 35, "glm/glm-4-plus": 35,
      "glm-4-flash": 15, "glm/glm-4-flash": 15, "glm-4-air": 20, "glm/glm-4-air": 20,
    },
  },
  {
    name: "minimax", Ctor: MiniMaxAnthropicProvider, defaultModel: "MiniMax-M2.7",
    policies: {
      "MiniMax-M2.7": 35, "MiniMax-M2.7-highspeed": 35, "MiniMax-M2.5": 25, "MiniMax-M2.5-highspeed": 25,
      "MiniMax-M2.1": 25, "MiniMax-M2.1-highspeed": 25, "MiniMax-M2": 20, "MiniMax-Text-01": 20,
    },
  },
]

describe("Anthropic-compatible vendor providers — golden master", () => {
  for (const c of CASES) {
    describe(c.name, () => {
      it("descriptor() at default model", () => {
        const p = new c.Ctor("test-key")
        expect(p.descriptor()).toEqual(baseDescriptor(c.name, c.defaultModel))
      })

      it("descriptor().provider is the vendor id for an arbitrary model", () => {
        const p = new c.Ctor("test-key", "some-custom-model")
        expect(p.descriptor()).toEqual(baseDescriptor(c.name, "some-custom-model"))
      })

      it("runtimePolicy() matches the known policy table for every model", () => {
        for (const [model, maxTurns] of Object.entries(c.policies)) {
          const p = new c.Ctor("test-key", model)
          expect(p.runtimePolicy()).toEqual({ maxTurns })
        }
      })

      it("runtimePolicy() is empty for an unknown model", () => {
        const p = new c.Ctor("test-key", "unknown-model-xyz")
        expect(p.runtimePolicy()).toEqual({})
      })
    })
  }
})
