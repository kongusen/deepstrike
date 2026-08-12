import { Agent } from "../../agent.js"
import { tool, type RegisteredTool } from "../../tools/index.js"

/** spc_007 §2: minimal OpenAI Agents SDK agent-definition shape this adapter accepts. Field
 *  names/shape mirror the doc's §2 table left column, not any live OpenAI SDK type import — this
 *  adapter reads a serialized agent definition (e.g. `__fixtures__/openai-agent.json`), not a
 *  running SDK object. */
export interface OpenAiAgentJson {
  name: string
  instructions?: string
  model?: string
  tools?: Array<{
    type?: string
    name: string
    description?: string
    parameters?: Record<string, unknown>
  }>
  handoffs?: Array<{ agent: string; description?: string }>
  /** OpenAI-specific: no common `Agent` field for this (spc_007 §2's "Guardrails" row lowers to
   *  governance policy, not a Canonical IR field) — preserved under `providerOptions.openai`
   *  rather than silently dropped (spc_001 §3: "Unknown fields → never silently discarded"). */
  guardrails?: unknown[]
}

/** spc_007 §2: OpenAI Agent → DeepStrike `Agent` (spc_001's Canonical Agent IR entry object).
 *  Pure Surface→Surface mapping — produces only `Agent`, never a Kernel-internal type
 *  (`Tcb`/`Capability`/etc.), matching this doc's own boundary ("Adapter 代码不得进入 Kernel
 *  crate").
 *
 *  A JSON tool definition carries no executable body, so each mapped tool's `execute` is a
 *  placeholder that throws — this adapter only reconstructs declarative shape (name/description/
 *  schema/count), never behavior; wiring a real implementation back onto an OpenAI-sourced tool
 *  definition is a separate, later concern outside this card. */
export function fromOpenAiAgent(json: OpenAiAgentJson): Agent {
  const tools: RegisteredTool[] = (json.tools ?? []).map(t =>
    tool(t.name, t.description ?? "", t.parameters ?? { type: "object", properties: {} }, async () => {
      throw new Error(
        `tool "${t.name}" has no execution binding — fromOpenAiAgent only carries declarative shape`,
      )
    }),
  )

  return new Agent({
    name: json.name,
    instructions: json.instructions,
    model: json.model,
    tools,
    handoffs: json.handoffs,
    ...(json.guardrails ? { providerOptions: { openai: { guardrails: json.guardrails } } } : {}),
  })
}
