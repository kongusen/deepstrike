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

/** Stable provider-failure contract. Only the named scalar fields cross the host ABI. */
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

const CONTEXT_OVERFLOW_CODES = new Set([
  "context_length_exceeded",
  "prompt_too_long",
])
const NETWORK_CODES = new Set([
  "ECONNABORTED",
  "ECONNREFUSED",
  "ECONNRESET",
  "EHOSTUNREACH",
  "ENETUNREACH",
  "ETIMEDOUT",
])

function object(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" ? value as Record<string, unknown> : undefined
}

function scalarString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined
}

function scalarStatus(value: unknown): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value >= 100 && value <= 599
    ? value
    : undefined
}

function errorStatus(error: unknown): number | undefined {
  const outer = object(error)
  const response = object(outer?.response)
  return scalarStatus(outer?.httpStatus)
    ?? scalarStatus(outer?.status)
    ?? scalarStatus(response?.status)
}

function errorCode(error: unknown): string | undefined {
  const outer = object(error)
  const nested = object(outer?.error)
  const nestedError = object(nested?.error)
  const body = object(outer?.body)
  const bodyError = object(body?.error)
  return scalarString(outer?.providerCode)
    ?? scalarString(outer?.code)
    ?? scalarString(outer?.error_code)
    ?? scalarString(nested?.code)
    ?? scalarString(nested?.error_code)
    ?? scalarString(nestedError?.code)
    ?? scalarString(nestedError?.error_code)
    ?? scalarString(nestedError?.type)
    ?? scalarString(bodyError?.code)
    ?? scalarString(bodyError?.error_code)
}

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message
  return typeof error === "string" ? error : "Provider request failed"
}

function classifyKind(error: unknown, status: number | undefined, code: string | undefined): ProviderErrorKind {
  if (code && CONTEXT_OVERFLOW_CODES.has(code.toLowerCase())) return "context_overflow"

  const name = error instanceof Error ? error.name : scalarString(object(error)?.name)
  if (name === "UnsupportedModalityError") return "modality"
  if (name === "ProtocolResponseError") return "protocol"
  if (name === "ContentValidationError" || name === "ProviderReplayValidationError") {
    return "invalid_request"
  }
  if (name === "APIConnectionError" || name === "APIConnectionTimeoutError") return "transport"

  if (status === 401 || status === 403) return "auth"
  if (status === 429) return "rate_limit"
  if (code === "model_not_found" || status === 404 || (status !== undefined && status >= 500)) {
    return "model_unavailable"
  }
  if (status === 408 || status === 409) return "transport"
  if (status === 400 || status === 422) return "invalid_request"

  const normalizedCode = code?.toUpperCase()
  if (normalizedCode && NETWORK_CODES.has(normalizedCode)) return "transport"
  if (error instanceof TypeError && status === undefined) return "transport"
  return "unknown"
}

function retryable(kind: ProviderErrorKind, status: number | undefined): boolean {
  if (kind === "transport" || kind === "rate_limit" || kind === "model_unavailable") return true
  if (kind === "unknown" && status !== undefined) return status >= 500 && status !== 501
  return false
}

export function classifyProviderError(provider: string, error: unknown): ProviderError {
  if (error instanceof ProviderError) return error
  const httpStatus = errorStatus(error)
  const providerCode = errorCode(error)
  const kind = classifyKind(error, httpStatus, providerCode)
  return new ProviderError({
    provider,
    kind,
    retryable: retryable(kind, httpStatus),
    message: errorMessage(error),
    ...(httpStatus !== undefined ? { httpStatus } : {}),
    ...(providerCode !== undefined ? { providerCode } : {}),
    cause: error,
  })
}

export function circuitOpenError(provider: string): ProviderError {
  return new ProviderError({
    provider,
    kind: "model_unavailable",
    retryable: true,
    providerCode: "circuit_open",
    message: "Circuit breaker open",
  })
}

/** Safe scalar projection for runner → canonical-host events. */
export function providerErrorEventFields(error: unknown): {
  error_kind?: ProviderErrorKind
  retryable?: boolean
  http_status?: number
  provider_code?: string
} {
  if (!(error instanceof ProviderError)) return {}
  return {
    error_kind: error.kind,
    retryable: error.retryable,
    ...(error.httpStatus !== undefined ? { http_status: error.httpStatus } : {}),
    ...(error.providerCode !== undefined ? { provider_code: error.providerCode } : {}),
  }
}

/** Provider errors expose their stable message without serializing the retained SDK cause. */
export function providerErrorMessage(error: unknown, fallback: (value: unknown) => string): string {
  return error instanceof ProviderError ? error.message : fallback(error)
}
