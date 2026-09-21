/** SPC-028-09: registered authority/projection relationships. */
export interface ProjectionPair {
  readonly authority: string
  readonly projection: string
  readonly crossingFunction: string
}

export const PROJECTION_PAIRS = {
  Message: { authority: "ModelMessage", projection: "WireMessage", crossingFunction: "adapter.encode/decode" },
  Content: { authority: "ContentPart[]", projection: "string", crossingFunction: "projectContentToText" },
  ToolResult: { authority: "contentParts", projection: "output", crossingFunction: "projectToolOutputToText" },
  StopReason: { authority: "GenerationProtocol stop reason", projection: "CanonicalStopReason", crossingFunction: "normalizeStopReason" },
  ToolCall: { authority: "ToolCall", projection: "ToolCallEvent", crossingFunction: "decodeToolCall" },
  ResourceQuota: { authority: "ResourceQuota", projection: "QuotaSnapshot", crossingFunction: "projectQuota" },
  TerminationReason: { authority: "Kernel terminal fact", projection: "RunResult.status", crossingFunction: "statusFromDone" },
} as const satisfies Record<string, ProjectionPair>
