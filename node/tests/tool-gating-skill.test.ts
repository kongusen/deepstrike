import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, StreamEvent, ToolSchema } from "../src/types.js"

/**
 * P1-B B3 end-to-end: loading a skill widens the canonical baseline on the next turn by exactly
 * its declared `allowed_tools`. Meta-tools remain reachable so more skills can load.
 */
function toolsPerTurnProvider(captured: string[][]): LLMProvider {
  let call = 0
  const record = (tools: ToolSchema[]) => captured.push(tools.map(t => t.name))
  return {
    async complete(_ctx, tools: ToolSchema[]): Promise<Message> {
      record(tools)
      return { role: "assistant", content: "done" }
    },
    async *stream(_ctx, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
      record(tools)
      call += 1
      if (call === 1) {
        yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
      } else {
        yield { type: "text_delta", delta: "done" }
      }
    },
  }
}

const baseTools = () => [
  tool("read", "read", { type: "object", properties: {} }, async () => "r"),
  tool("write", "write", { type: "object", properties: {} }, async () => "w"),
  tool("bash", "bash", { type: "object", properties: {} }, async () => "b"),
  tool("grep", "grep", { type: "object", properties: {} }, async () => "g"),
]

describe("P1-B B3: skill-activated tool gating (end-to-end)", () => {
  it("widens the exposed toolset after a skill with allowed_tools loads", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-gate-skill-"))
    await writeFile(
      join(dir, "debug.md"),
      "---\nname: debug\ndescription: Debug helper\nallowed_tools: read, grep\n---\nDebug guidance.",
    )

    const perTurn: string[][] = []
    const { runner } = createRunner(toolsPerTurnProvider(perTurn), baseTools(), {
      skillDir: dir,
      baselineToolIds: ["read"],
      stableCoreToolIds: ["bash"], // always exposed under gating
    })
    await collectText(runner.run({ sessionId: "gate-skill", goal: "debug it" }))

    expect(perTurn.length).toBeGreaterThanOrEqual(2)
    const loadTurn = perTurn[0]
    const afterTurn = perTurn[perTurn.length - 1]

    // Turn 1: canonical baseline + stable-core + meta.
    expect(loadTurn).toEqual(expect.arrayContaining(["read", "bash", "skill"]))
    expect(loadTurn).not.toContain("write")
    expect(loadTurn).not.toContain("grep")

    // Turn 2: the active skill adds grep; unrelated write remains hidden.
    expect(afterTurn).toEqual(expect.arrayContaining(["read", "grep", "bash", "skill"]))
    expect(afterTurn).not.toContain("write")
  })

  it("keeps the minimal baseline when the skill load fails", async () => {
    // The provider loads "debug", but this dir has no such skill ⇒ the load errors ⇒ no activation
    // ⇒ no widening. Failed/missing skills cannot expand exposure.
    const dir = await mkdtemp(join(tmpdir(), "ds-gate-miss-"))
    await writeFile(join(dir, "other.md"), "---\nname: other\ndescription: x\nallowed_tools: read\n---\nbody")

    const perTurn: string[][] = []
    const { runner } = createRunner(toolsPerTurnProvider(perTurn), baseTools(), {
      skillDir: dir,
      baselineToolIds: ["read"],
      stableCoreToolIds: ["bash"],
    })
    await collectText(runner.run({ sessionId: "gate-miss", goal: "go" }))
    for (const t of perTurn) {
      expect(t).toEqual(expect.arrayContaining(["read", "bash"]))
      expect(t).not.toContain("write")
      expect(t).not.toContain("grep")
    }
  })
})

/**
 * `skillFilter` is a host-layer allowlist over the scanned catalog by skill NAME. It executes at
 * the `scanSkillDir → set_available_skills` feed, so its effect is observable in the model-facing
 * catalog: the `skill` meta-tool's description embeds an `<available_skills>` block listing exactly the
 * fed skills. Absent ⇒ all scanned skills advertised (zero behavior difference); a list ⇒ only named
 * skills; `[]` ⇒ none. (Skill FILE activation reads from disk directly, so it is NOT a proxy for the
 * feed — we assert the advertised catalog itself.)
 */
describe("skillFilter host allowlist over the skill catalog", () => {
  // A provider that only records the exposed schemas (no tool calls) — turn 0 carries the full catalog.
  function schemaRecorder(captured: ToolSchema[][]): LLMProvider {
    return {
      async complete(_ctx, tools: ToolSchema[]): Promise<Message> {
        captured.push(tools); return { role: "assistant", content: "done" }
      },
      async *stream(_ctx, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
        captured.push(tools); yield { type: "text_delta", delta: "done" }
      },
    }
  }

  // A two-skill catalog so "keep only the named one" is distinguishable from "keep all".
  async function twoSkillCatalog() {
    const dir = await mkdtemp(join(tmpdir(), "ds-skillfilter-"))
    await writeFile(join(dir, "debug.md"), "---\nname: debug\ndescription: Debug helper\n---\nDebug guidance.")
    await writeFile(join(dir, "noise.md"), "---\nname: noise\ndescription: Unrelated helper\n---\nNoise guidance.")
    return dir
  }

  /** The `<available_skills>` catalog advertised on turn 0. */
  async function advertisedSkills(skillFilter: string[] | undefined): Promise<string> {
    const dir = await twoSkillCatalog()
    const captured: ToolSchema[][] = []
    const { runner } = createRunner(schemaRecorder(captured), baseTools(), {
      skillDir: dir,
      ...(skillFilter === undefined ? {} : { skillFilter }),
    })
    await collectText(runner.run({ sessionId: "skillfilter", goal: "go" }))
    const skillTool = captured[0].find(t => t.name === "skill")
    return skillTool?.description ?? ""
  }

  it("absent ⇒ every scanned skill advertised (zero behavior difference)", async () => {
    const desc = await advertisedSkills(undefined)
    expect(desc).toContain("<name>debug</name>")
    expect(desc).toContain("<name>noise</name>")
  })

  it("a list keeps ONLY the named skills (debug in, noise filtered out)", async () => {
    const desc = await advertisedSkills(["debug"])
    expect(desc).toContain("<name>debug</name>")
    expect(desc).not.toContain("<name>noise</name>")
  })

  it("empty array ⇒ NO skills advertised (message shape preserved, list empty)", async () => {
    const desc = await advertisedSkills([])
    expect(desc).not.toContain("<name>debug</name>")
    expect(desc).not.toContain("<name>noise</name>")
  })
})
