/** Public guardrail declaration. Execution/lowering belongs to a later governance card. */
export interface Guardrail {
  name: string
  description?: string
  metadata?: Record<string, unknown>
}
