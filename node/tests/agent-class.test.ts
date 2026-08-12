import { Agent } from "../src/agent.js"
import { tool } from "../src/tools/index.js"
import { fromAnthropicMcpConfig } from "../src/compat/anthropic/mcp.js"

describe("spc_001-02: Agent public class", () => {
  it("stores constructor fields and exposes them as read properties", () => {
    const someTool = tool("noop", "does nothing", { type: "object", properties: {} }, () => "ok")
    const agent = new Agent({
      name: "researcher",
      description: "digs up facts",
      instructions: "be thorough",
      model: "claude-opus-5",
      tools: [someTool],
      providerOptions: { openai: { reasoningEffort: "high" } },
    })

    expect(agent.name).toBe("researcher")
    expect(agent.description).toBe("digs up facts")
    expect(agent.instructions).toBe("be thorough")
    expect(agent.model).toBe("claude-opus-5")
    expect(agent.tools).toEqual([someTool])
    expect(agent.providerOptions).toEqual({ openai: { reasoningEffort: "high" } })
  })

  it("leaves optional fields undefined when omitted", () => {
    const agent = new Agent({ name: "minimal" })
    expect(agent.description).toBeUndefined()
    expect(agent.tools).toBeUndefined()
    expect(agent.mcpServers).toBeUndefined()
    expect(agent.skills).toBeUndefined()
    expect(agent.knowledge).toBeUndefined()
    expect(agent.handoffs).toBeUndefined()
  })
})

describe("spc_009-08: Agent.mcpServers is MCPServer[], not unknown[]", () => {
  it("accesses a field only MCPServer[] exposes without an `as` assertion", () => {
    const server = fromAnthropicMcpConfig({ name: "fs", command: "npx", args: ["mcp-fs"] })
    const agent = new Agent({ name: "researcher", mcpServers: [server] })

    // With `mcpServers?: unknown[]`, `agent.mcpServers[0].transport` does not type-check without
    // an `as MCPServer` cast first — this line is the type-level proof the field narrowed.
    const transport = agent.mcpServers?.[0].transport
    expect(transport?.kind).toBe("stdio")
    expect(agent.mcpServers).toEqual([server])
  })
})

describe("spc_001-06: Agent.model dual-mode (ModelRef)", () => {
  it("accepts a bare string model name (zero regression on the existing string form)", () => {
    const agent = new Agent({ name: "researcher", model: "claude-opus-5" })
    expect(agent.model).toBe("claude-opus-5")
  })

  it("accepts a ModelRequirement object and stores it as-is", () => {
    const agent = new Agent({ name: "researcher", model: { capability: { reasoning: true } } })
    expect(agent.model).toEqual({ capability: { reasoning: true } })
  })
})

describe("spc_010-01: Agent output schema and metadata", () => {
  it("stores an output schema and metadata without interpreting either", () => {
    const outputSchema = {
      type: "object",
      required: ["answer"],
      properties: { answer: { type: "string" } },
    }
    const metadata = { team: "research", priority: 1 }
    const agent = new Agent({ name: "researcher", outputSchema, metadata })

    expect(agent.outputSchema).toEqual(outputSchema)
    expect(agent.metadata).toEqual(metadata)
  })

  it("leaves output schema and metadata undefined when omitted", () => {
    const agent = new Agent({ name: "minimal" })

    expect(agent.outputSchema).toBeUndefined()
    expect(agent.metadata).toBeUndefined()
  })
})

describe("spc_010-02: Agent guardrails", () => {
  it("stores typed guardrails without assigning execution semantics", () => {
    const guardrails = [{
      name: "no-pii",
      description: "do not return personal data",
      metadata: { severity: "high" },
    }]
    const agent = new Agent({ name: "researcher", guardrails })

    expect(agent.guardrails).toEqual(guardrails)
  })

  it("leaves guardrails undefined when omitted", () => {
    expect(new Agent({ name: "minimal" }).guardrails).toBeUndefined()
  })
})
