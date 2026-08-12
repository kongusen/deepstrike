import { Agent } from "../src/agent.js"
import type { Knowledge } from "../src/knowledge/public.js"
import type { KnowledgeSource } from "../src/knowledge/source.js"
import type { Handoff } from "../src/handoff-target.js"
import type { HandoffArtifact } from "../src/collaboration/handoff.js"

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
