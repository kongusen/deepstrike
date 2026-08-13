import { readFileSync } from "node:fs"
import { join } from "node:path"

import { CanonicalKernel } from "@deepstrike/wasm-kernel"

type CanonicalFixture = {
  links: Array<{
    envelope: Record<string, unknown>
    step: Record<string, unknown>
  }>
}

function readCanonicalFixture(name: string): CanonicalFixture {
  return JSON.parse(readFileSync(
    join(process.cwd(), "../tests/fixtures/kernel-wire", name),
    "utf8",
  )) as CanonicalFixture
}

describe("WASM canonical ABI fixtures", () => {
  it("drives the canonical agent-root lifecycle", () => {
    const fixture = readCanonicalFixture("golden_lifecycle_agent_root.json")
    const kernel = new CanonicalKernel()

    for (const [index, link] of fixture.links.entries()) {
      expect(link.envelope).not.toHaveProperty("abi_version")
      const prepared = kernel.prepare(JSON.stringify(link.envelope))
      expect(prepared.status).toBe("prepared")
      if (prepared.status !== "prepared") throw new Error("expected prepared transition")
      const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
      expect(committed.stepSeq).toBe(String(index))
    }

    expect(kernel.lifecycle()).toBe("running")
    const pending = JSON.parse(kernel.pendingEffectsJson()) as Array<{
      effect?: { kind?: string }
    }>
    expect(pending[0]?.effect?.kind).toBe("call_provider")
  })

  it("uses one atomic start_operation for a workflow root", () => {
    const fixture = readCanonicalFixture("golden_lifecycle_workflow_root.json")
    const start = fixture.links[1]?.envelope.input as {
      kind?: string
      entry?: { kind?: string }
    }

    expect(start.kind).toBe("start_operation")
    expect(start.entry?.kind).toBe("workflow")
    expect(JSON.stringify(fixture)).not.toMatch(/load_workflow|complete_run/)
  })
})
