import type { ContentPart, Message, RenderedContext } from "../types.js"
import { getModelProfile, modelProfiles, type ModelProfileId, type ProviderId } from "./profiles.js"
import { UnsupportedModalityError } from "./base.js"

interface ModelCapabilities {
  contextWindow: number
  maxOutputTokens?: number
  inputModalities: Set<"text" | "image" | "audio" | "video" | "file">
  outputModalities: Set<"text">
  tools: boolean
  parallelToolCalls?: boolean
  structuredOutput?: boolean
  reasoning?: boolean
  promptCaching?: boolean
  tokenCounting: "native" | "local-exact" | "heuristic"
  media: { imageUrl?: boolean; imageBase64?: boolean; fileId?: boolean; audioUrl?: boolean; audioBase64?: boolean }
}

/**
 * spc_011-D-01: projects `ModelCapabilities` from the existing `modelProfiles` table
 * (`reference_model_registry_not_whitelist` re-verified: it's advisory metadata with zero runtime
 * consumers outside `catalog.ts`'s endpoint routing, so extending it is safe — nothing enforces
 * against it today). `ModelProfile.modalities` used "pdf"; this type uses "file" per §7's literal
 * shape, translated below rather than renaming the existing (also still-unused) vocabulary.
 *
 * Models without a Track-D overlay (maxOutputTokens/promptCaching/tokenCounting/media) fall back to
 * conservative defaults — `tokenCounting: "heuristic"` and an all-unconfirmed `media` object — rather
 * than fabricating vendor claims for models nobody has audited yet.
 */
function projectModelCapabilities(id: ModelProfileId): ModelCapabilities {
  const profile = getModelProfile(id)
  return {
    contextWindow: profile.contextWindow ?? 0,
    maxOutputTokens: profile.maxOutputTokens,
    inputModalities: new Set(profile.modalities.input.map(m => (m === "pdf" ? "file" : m))),
    outputModalities: new Set(profile.modalities.output.filter((m): m is "text" => m === "text")),
    tools: profile.tools.supported,
    parallelToolCalls: profile.parallelToolCalls,
    structuredOutput: profile.structuredOutput,
    reasoning: profile.reasoning.supported,
    promptCaching: profile.promptCaching,
    tokenCounting: profile.tokenCounting ?? "heuristic",
    media: profile.media ?? {},
  }
}

/**
 * Transitional internal lookup keyed by the (providerId, bare model
 * name) pair providers actually hold at runtime (e.g. `descriptor().provider`/`descriptor().model`
 * — `this.model` on a provider instance is never prefixed) instead of the `"provider/model"`
 * `ModelProfileId` string. Returns `undefined` for custom/unregistered model names rather than
 * throwing — per `reference_model_registry_not_whitelist`, any vendor model id is expected to
 * work with zero registry entry, so there is no capability data to enforce against for those and
 * failing closed on missing data would regress that "any model id works" contract.
 */
export function tryGetModelCapabilities(providerId: ProviderId | string, model: string): ModelCapabilities | undefined {
  const id = `${providerId}/${model}`
  if (!Object.prototype.hasOwnProperty.call(modelProfiles, id)) return undefined
  return projectModelCapabilities(id as ModelProfileId)
}

const CONTENT_PART_MODALITY: Partial<Record<ContentPart["type"], "image" | "audio">> = {
  image: "image",
  audio: "audio",
}

/**
 * spc_011-D-02 (invariant 2): throws `UnsupportedModalityError` for the first content part whose
 * modality isn't in `capabilities.inputModalities`, instead of letting it reach the wire. Text and
 * tool-result parts are never modality-checked. A `capabilities` of `undefined` (unregistered
 * model, see `tryGetModelCapabilities`) is a deliberate no-op — there is no data to enforce.
 */
export function assertModalitySupported(
  turns: Message[],
  capabilities: ModelCapabilities | undefined,
  providerName: string,
): void {
  if (!capabilities) return
  for (const turn of turns) {
    for (const part of turn.contentParts ?? []) {
      const modality = CONTENT_PART_MODALITY[part.type]
      if (modality && !capabilities.inputModalities.has(modality)) {
        throw new UnsupportedModalityError(modality, providerName)
      }
    }
  }
}

/** Convenience wrapper over `assertModalitySupported` for the common case of checking every
 *  turn in a `RenderedContext` (history + the volatile state turn, if present). */
export function assertContextModalitySupported(
  context: RenderedContext,
  capabilities: ModelCapabilities | undefined,
  providerName: string,
): void {
  assertModalitySupported(
    context.stateTurn ? [...context.turns, context.stateTurn] : context.turns,
    capabilities,
    providerName,
  )
}
