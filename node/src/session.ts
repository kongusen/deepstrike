/**
 * Public user-session descriptor. This is distinct from persisted `SessionData` and the runtime
 * `SessionLog`: it carries caller-visible continuity metadata, not messages or journal events.
 */
export interface Session {
  id: string
  userId?: string
  state?: Record<string, unknown>
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}
