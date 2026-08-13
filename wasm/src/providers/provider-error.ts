export type ProviderErrorKind =
  | "transport"
  | "auth"
  | "rate_limit"
  | "context_overflow"
  | "invalid_request"
  | "modality"
  | "model_unavailable"
  | "protocol"
  | "unknown"

export interface ProviderErrorOptions {
  provider: string
  kind: ProviderErrorKind
  retryable: boolean
  message: string
  httpStatus?: number
  providerCode?: string
  cause?: unknown
}

/** The only provider-failure shape allowed across the runner/kernel boundary. */
export class ProviderError extends Error {
  readonly provider: string
  readonly kind: ProviderErrorKind
  readonly retryable: boolean
  readonly httpStatus?: number
  readonly providerCode?: string
  override readonly cause: unknown

  constructor(options: ProviderErrorOptions) {
    super(options.message, options.cause === undefined ? undefined : { cause: options.cause })
    this.name = "ProviderError"
    this.provider = options.provider
    this.kind = options.kind
    this.retryable = options.retryable
    this.httpStatus = options.httpStatus
    this.providerCode = options.providerCode
    this.cause = options.cause
  }
}

const CONTEXT_OVERFLOW_CODES = new Set(["context_length_exceeded", "prompt_too_long"])
const NETWORK_CODES = new Set(["ECONNABORTED", "ECONNREFUSED", "ECONNRESET", "EHOSTUNREACH", "ENETUNREACH", "ETIMEDOUT"])

function object(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" ? value as Record<string, unknown> : undefined
}

function scalarString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined
}

function scalarStatus(value: unknown): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value >= 100 && value <= 599 ? value : undefined
}

function status(error: unknown): number | undefined {
  const outer = object(error)
  const response = object(outer?.response)
  return scalarStatus(outer?.httpStatus) ?? scalarStatus(outer?.status) ?? scalarStatus(response?.status)
}

function code(error: unknown): string | undefined {
  const outer = object(error)
  const nested = object(outer?.error)
  const bodyError = object(object(outer?.body)?.error)
  return scalarString(outer?.providerCode)
    ?? scalarString(outer?.code)
    ?? scalarString(outer?.error_code)
    ?? scalarString(nested?.code)
    ?? scalarString(nested?.error_code)
    ?? scalarString(bodyError?.code)
    ?? scalarString(bodyError?.error_code)
}

function kind(error: unknown, httpStatus: number | undefined, providerCode: string | undefined): ProviderErrorKind {
  if (httpStatus === 413 || (providerCode && CONTEXT_OVERFLOW_CODES.has(providerCode.toLowerCase()))) return "context_overflow"
  if (httpStatus === 401 || httpStatus === 403) return "auth"
  if (httpStatus === 429) return "rate_limit"
  if (providerCode === "model_not_found" || httpStatus === 404 || (httpStatus !== undefined && httpStatus >= 500)) return "model_unavailable"
  if (httpStatus === 408 || httpStatus === 409) return "transport"
  if (httpStatus === 400 || httpStatus === 422) return "invalid_request"
  if (providerCode && NETWORK_CODES.has(providerCode.toUpperCase())) return "transport"
  if (error instanceof TypeError && httpStatus === undefined) return "transport"
  return "unknown"
}

export function classifyProviderError(provider: string, error: unknown): ProviderError {
  if (error instanceof ProviderError) return error
  const httpStatus = status(error)
  const providerCode = code(error)
  const classified = kind(error, httpStatus, providerCode)
  return new ProviderError({
    provider,
    kind: classified,
    retryable: classified === "transport" || classified === "rate_limit" || classified === "model_unavailable",
    message: error instanceof Error && error.message ? error.message : typeof error === "string" ? error : "Provider request failed",
    ...(httpStatus !== undefined ? { httpStatus } : {}),
    ...(providerCode !== undefined ? { providerCode } : {}),
    cause: error,
  })
}

export function providerErrorEventFields(error: ProviderError): Record<string, unknown> {
  return {
    error_kind: error.kind,
    retryable: error.retryable,
    ...(error.httpStatus !== undefined ? { http_status: error.httpStatus } : {}),
    ...(error.providerCode !== undefined ? { provider_code: error.providerCode } : {}),
  }
}
