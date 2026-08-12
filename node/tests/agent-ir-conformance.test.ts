import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { Agent } from "../src/agent.js"
import { lowerAgent, normalizeAgent, type AgentDefinition } from "../src/agent-ir.js"
import { fromOpenAiAgent, type OpenAiAgentJson } from "../src/compat/openai/agent.js"
import { fromAnthropicMcpConfig } from "../src/compat/anthropic/mcp.js"

async function fixture(): Promise<AgentDefinition> {
  return JSON.parse(await readFile(join(process.cwd(), "..", "tests", "fixtures", "agent-ir", "v1-agent.json"), "utf8")) as AgentDefinition
}

describe("spc_015-09: Canonical Agent IR", () => {
  it("normalizes the shared fixture without dropping portable, DeepStrike, or unknown namespaced fields", async () => {
    const agent = normalizeAgent(await fixture())
    const spec = lowerAgent(agent)

    expect(spec.version).toBe(1)
    expect(spec.name).toBe("researcher")
    expect(spec.description).toBe("Finds and verifies source material.")
    expect(spec.instructions).toBe("Cite primary sources and state uncertainty.")
    expect(spec.model).toEqual({
      capability: { reasoning: true, vision: true, toolUse: true },
      contextWindow: 128000,
      latencyClass: "balanced",
      costClass: "standard",
    })
    expect(spec.outputSchema).toEqual({
      type: "object",
      properties: { answer: { type: "string" } },
      required: ["answer"],
    })
    expect(spec.tools).toEqual([{
      name: "web_search",
      description: "Search the web for source material.",
      parameters: {
        type: "object",
        properties: { query: { type: "string" } },
        required: ["query"],
      },
      providerOptions: { openai: { strict: true } },
    }])
    expect(spec.memory).toEqual({ kind: "durable", namespace: "project-research" })
    expect(spec.mcpServers?.[0].providerOptions).toEqual({ anthropic: { toolSearch: true } })
    expect(spec.skills?.[0].providerOptions).toEqual({ custom: { revision: 2 } })
    expect(spec.knowledge?.[0].source).toEqual({ kind: "text", content: "Use concise citations." })
    expect(spec.handoffs?.[0].agent).toBe("writer")
    expect(spec.guardrails?.[0].name).toBe("no-pii")
    expect(spec.metadata).toEqual({ team: "research", priority: 1 })
    expect(spec.extensions).toEqual({
      openai: { reasoningEffort: "high" },
      "example.future_provider": { opaque: { preserve: true } },
    })
    expect(spec.providerOptions).toEqual(spec.extensions)

    expect(spec.inputs.context.knowledge).toEqual(spec.knowledge)
    expect(spec.inputs.capabilities.tools).toEqual(spec.tools)
    expect(spec.inputs.capabilities.mcpServers).toEqual(spec.mcpServers)
    expect(spec.inputs.capabilities.skills).toEqual(spec.skills)
    expect(spec.inputs.memory).toEqual(spec.memory)
    expect(spec.inputs.delegation.handoffs).toEqual(spec.handoffs)
    expect(spec.inputs.governance.guardrails).toEqual(spec.guardrails)
    expect(spec.capabilityFilter).toEqual({
      allowedKinds: ["tool", "skill", "mcp_server"],
      allowedIds: ["web_search", "citations"],
    })
    expect(spec.effectiveCapabilities).toEqual([
      { kind: "tool", id: "web_search", description: "Search the web for source material." },
      { kind: "skill", id: "citations", description: "Citation policy." },
    ])
    expect(spec.inputs.capabilities.effective).toEqual(spec.effectiveCapabilities)
  })

  it("keeps the canonical IR independent of later mutations to the public surface", async () => {
    const agent = normalizeAgent(await fixture())
    const spec = lowerAgent(agent)
    agent.providerOptions!.openai = { reasoningEffort: "low" }
    agent.metadata!.team = "changed"

    expect(spec.extensions.openai).toEqual({ reasoningEffort: "high" })
    expect(spec.metadata).toEqual({ team: "research", priority: 1 })
  })

  it("normalizes native, OpenAI-shaped, and Anthropic-MCP surfaces before lowering", async () => {
    const native = normalizeAgent(await fixture())
    const openaiRaw = JSON.parse(await readFile(join(process.cwd(), "src", "__fixtures__", "openai-agent.json"), "utf8")) as OpenAiAgentJson
    const openai = normalizeAgent(fromOpenAiAgent(openaiRaw))
    const anthropic = normalizeAgent(new Agent({
      name: "filesystem-agent",
      mcpServers: [fromAnthropicMcpConfig({ name: "filesystem", command: "mcp-filesystem", args: ["/workspace"] })],
    }))

    expect(lowerAgent(native).tools[0].name).toBe("web_search")
    expect(lowerAgent(openai).tools[0].name).toBe("web_search")
    expect(lowerAgent(openai).handoffs?.[0].agent).toBe("writer")
    expect(lowerAgent(anthropic).mcpServers?.[0]).toEqual({
      name: "filesystem",
      transport: { kind: "stdio", command: "mcp-filesystem", args: ["/workspace"] },
    })
  })
})
