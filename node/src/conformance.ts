// SDK conformance harness surface. Kept separate from the public root so the
// executable SDK contract does not accidentally grow internal protocol exports.
export { decodeDurableContent, decodeDurableToolResult } from "./runtime/durable-content.js"
export { decodeCanonicalContentParts, encodeCanonicalContentParts } from "./runtime/kernel-step.js"
export { SESSION_EVENT_KINDS } from "./runtime/session-log.js"
export { providerAttemptToRecord } from "./runtime/execution-evidence.js"
