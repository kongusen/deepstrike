import { Agent } from "../src/agent.js"
import type { Knowledge } from "../src/knowledge/public.js"
import type { KnowledgeSource } from "../src/knowledge/source.js"

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
