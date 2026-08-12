import { Agent } from "../src/agent.js"
import type { Knowledge } from "../src/knowledge/public.js"
import type { KnowledgeSource } from "../src/knowledge/source.js"
import type { Handoff } from "../src/handoff-target.js"
import type { HandoffArtifact } from "../src/collaboration/handoff.js"
import type { Session } from "../src/session.js"
import type { SessionData } from "../src/memory/protocols.js"
import type { SessionLog } from "../src/runtime/session-log.js"
import type { MCPServer } from "../src/mcp-server.js"
import type { KnowledgeSourceRef, McpTransport } from "../src/index.js"

describe("spc_001: public descriptor provider options", () => {
  it("preserves vendor extensions on public descriptor contracts", () => {
    const mcp: MCPServer = {
      transport: { kind: "http", url: "https://example.test/mcp" },
      providerOptions: { openai: { approvalMode: "never" } },
    }
    const knowledge: Knowledge = {
      source: { kind: "text", content: "facts" },
      providerOptions: { anthropic: { citations: true } },
    }
    const handoff: Handoff = {
      agent: "reviewer",
      providerOptions: { google: { transferMode: "explicit" } },
    }
    const session: Session = {
      id: "session-provider-options",
      providerOptions: { custom: { tenant: "acme" } },
    }
    const source: KnowledgeSourceRef = { kind: "text", content: "root entrypoint" }
    const transport: McpTransport = { kind: "http", url: "https://example.test/root" }

    expect(JSON.parse(JSON.stringify(mcp)).providerOptions).toEqual({ openai: { approvalMode: "never" } })
    expect(JSON.parse(JSON.stringify(knowledge)).providerOptions).toEqual({ anthropic: { citations: true } })
    expect(JSON.parse(JSON.stringify(handoff)).providerOptions).toEqual({ google: { transferMode: "explicit" } })
    expect(JSON.parse(JSON.stringify(session)).providerOptions).toEqual({ custom: { tenant: "acme" } })
    expect(source.kind).toBe("text")
    expect(transport.kind).toBe("http")
  })
})

describe("spc_010-04: typed Agent knowledge", () => {
  it("stores every public knowledge source form without changing KnowledgeSource", async () => {
    const vectorRetriever: KnowledgeSource = {
      async init() {},
      async retrieve(_goal: string) { return ["result"] },
    }
    const knowledge: Knowledge[] = [
      { name: "file", source: { kind: "file", path: "/tmp/guide.md" } },
      { name: "directory", source: { kind: "directory", path: "/tmp/docs" } },
      { name: "text", source: { kind: "text", content: "facts" } },
      { name: "url", source: { kind: "url", url: "https://example.test/docs" } },
      { name: "vector", source: { kind: "vector", retriever: vectorRetriever } },
      { name: "custom", source: { kind: "custom", connector: "internal" } },
    ]
    const agent = new Agent({ name: "researcher", knowledge })

    expect(agent.knowledge?.map(item => item.source.kind)).toEqual([
      "file", "directory", "text", "url", "vector", "custom",
    ])
    expect(await vectorRetriever.retrieve("goal")).toEqual(["result"])
  })
})

describe("spc_010-05: typed Agent handoffs", () => {
  it("stores control-transfer descriptors without conflating them with sprint artifacts", () => {
    const handoffs: Handoff[] = [{
      agent: { name: "reviewer" },
      description: "review the result",
      inputSchema: { type: "object", required: ["draft"] },
    }]
    const agent = new Agent({ name: "writer", handoffs })

    expect(agent.handoffs?.[0].inputSchema).toEqual({ type: "object", required: ["draft"] })
    const artifact: Pick<HandoffArtifact, "goal" | "sprint"> = { goal: "ship", sprint: 1 }
    expect(artifact.goal).toBe("ship")
  })
})

describe("spc_010-06: public Session type", () => {
  it("describes user session state without conflating persistence record or runtime log", () => {
    const session: Session = {
      id: "session-1",
      userId: "user-1",
      state: { draft: true },
      metadata: { source: "web" },
    }
    const data: SessionData = {
      sessionId: "persisted-1",
      agentId: "agent-1",
      messages: [],
      metadata: null,
      createdAtMs: 1,
      updatedAtMs: 1,
    }
    const log: Pick<SessionLog, "append" | "read"> = {
      append: async () => 0,
      read: async () => [],
    }

    expect(session.state).toEqual({ draft: true })
    expect(data.messages).toEqual([])
    expect(typeof log.read).toBe("function")
  })
})
