/**
 * L3 — Skills handbook + Knowledge.
 *
 * L1's sourced agent, now with two capability-plane mechanisms:
 *
 *   • SKILLS (on-demand capability + tool gating). A `skillDir` catalog exposes a `skill`
 *     meta-tool. The catalog carries only each skill's *metadata*; the body loads lazily when the
 *     model calls `skill(name)`. Loading `citation-style` narrows the exposed toolset to
 *     `stableCore ∪ allowed_tools` — so the off-task `list_index` tool DISAPPEARS while the skill is
 *     active. `onTurnMetrics` prints `toolsExposed` per turn so you can watch the surface shrink.
 *
 *   • KNOWLEDGE (durable pinned partition). A `KnowledgeSource` is queried once at run start and its
 *     hits are pinned into the knowledge slot at the front of context — distinct from a skill body
 *     (loaded by the model, gated, lease-swept) and from memory (recalled, decaying). Here it pins
 *     the studio's non-negotiable style rule.
 *
 * New mechanisms: Skills, tool gating, Knowledge. Reused: tools, execution plane, provider (L1).
 *
 * Run:  npx tsx 03-skills-handbook/main.ts        (or --dry-run)
 */
import { RuntimeRunner, LocalExecutionPlane, InMemorySessionLog, tool } from "@deepstrike/sdk"
import type { RegisteredTool } from "@deepstrike/sdk"
import type { KnowledgeSource } from "@deepstrike/sdk/memory"
import { fileURLToPath } from "node:url"
import { dirname, join } from "node:path"
import { studioTools, CORPUS } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"
import { render } from "../shared/render.js"

const here = dirname(fileURLToPath(import.meta.url))

/** Formats a source id into the studio's canonical `[Title — id]` citation. Allowed by the skill. */
function formatCitationTool(): RegisteredTool {
  return tool(
    "format_citation",
    "Render a source id into the studio's canonical citation form `[Title — id]`. Use for EVERY cited claim.",
    { type: "object", properties: { id: { type: "string", description: "A source id you have read." } }, required: ["id"] },
    (args) => {
      const src = CORPUS.find((s) => s.id === String(args.id ?? ""))
      return src ? `[${src.title} — ${src.id}]` : `[unknown source '${args.id}']`
    },
  )
}

/** Lists every source id + title. Off-task once you're WRITING — so the skill gates it away. */
function listIndexTool(): RegisteredTool {
  return tool(
    "list_index",
    "List every source in the studio index as {id, title}. A browsing aid, not a citation tool.",
    { type: "object", properties: {} },
    () => JSON.stringify(CORPUS.map((s) => ({ id: s.id, title: s.title }))),
  )
}

/** A tiny static KnowledgeSource: one pinned house rule, retrieved at run start. A real one would
 *  wrap a vector index or a docs API; the contract is just `init()` + `retrieve(goal, topK)`. */
const styleGuide: KnowledgeSource = {
  async init() {},
  async retrieve() {
    return ["STUDIO STYLE (non-negotiable): every factual sentence in a brief must carry a citation produced by the format_citation tool; uncited claims are rejected in review."]
  },
}

async function main(): Promise<void> {
  loadEnv()
  const { flags } = parseArgs(process.argv.slice(2))
  const dryRun = flags["dry-run"] === true

  const plane = new LocalExecutionPlane()
  const tools = [...studioTools(), formatCitationTool(), listIndexTool()]
  for (const t of tools) plane.register(t)

  if (dryRun) {
    console.log("● L3 wiring check (no provider call)")
    console.log(`  skill dir      : ${join(here, "skills")}  → 'skill' meta-tool over the catalog`)
    console.log(`  base tools     : ${tools.map((t) => t.schema.name).join(", ")}`)
    console.log(`  stable core    : search, read_source  (always exposed)`)
    console.log(`  skill gating   : citation-style allows [format_citation] → list_index hides while active`)
    console.log(`  knowledge      : styleGuide  → 1 pinned rule retrieved at run start`)
    console.log("  ✓ set a key and drop --dry-run to watch toolsExposed shrink when the skill loads.")
    return
  }

  const runner = new RuntimeRunner({
    provider: resolveProvider(),
    executionPlane: plane,
    sessionLog: new InMemorySessionLog(),
    skillDir: join(here, "skills"),
    stableCoreToolIds: ["search", "read_source"], // survive skill gating; everything else is gated
    knowledgeSource: styleGuide,
    maxTokens: 200_000,
    maxTurns: 14,
    // Tool-gating telemetry: watch the exposed surface shrink the turn the skill activates.
    onTurnMetrics: (m) =>
      console.log(
        `\n  · turn ${m.turn}: exposed=${m.toolsExposed} called=${m.toolsCalled} skill=${m.activeSkill ?? "—"}`,
      ),
  })

  console.log("━━ write a cited brief ━━ (the agent loads the citation-style skill, then writes)\n")
  for await (const event of runner.run({
    sessionId: "l3-brief",
    goal:
      "Load the citation-style skill first. Then, using ONLY the studio index, write a two-sentence brief on " +
      "how prompt caching stays effective across turns. Cite every claim with format_citation and end with a Sources: line.",
  })) {
    render(event)
  }

  console.log(
    "\nNote the turn where `skill=citation-style` appears: `exposed` drops because `list_index` is " +
      "gated away — only stable-core (search, read_source) + the skill's format_citation remain.",
  )
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
