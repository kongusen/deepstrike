import { readFileSync } from "node:fs"
import { join } from "node:path"

import { getKernel } from "../src/kernel.js"

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

describe("Node canonical ABI fixtures", () => {
  it.each([
    ["golden_lifecycle_agent_root.json", "running"],
    ["golden_lifecycle_workflow_root.json", "completed"],
  ])("drives %s", (name, expectedLifecycle) => {
    const native = getKernel()
    const fixture = readCanonicalFixture(name)
    const kernel = new native.CanonicalKernel()

    for (const [index, link] of fixture.links.entries()) {
      expect(link.envelope.abi_version).toBe(native.kernelAbiVersion())
      const prepared = kernel.prepare(JSON.stringify(link.envelope))
      expect(prepared.status).toBe("prepared")
      if (prepared.status !== "prepared") throw new Error("expected prepared transition")
      expect(JSON.parse(prepared.plannedStepJson)).toEqual(link.step)
      const committed = kernel.commit(prepared.prepareToken, prepared.recordDigest)
      expect(committed.stepSeq).toBe(String(index))
    }

    expect(kernel.lifecycle()).toBe(expectedLifecycle)
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
