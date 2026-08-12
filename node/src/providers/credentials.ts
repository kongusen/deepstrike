import type { EndpointProfileId, EndpointProtocol, ProviderId } from "./endpoints.js"

export type ProviderCredential =
  | { type: "api_key"; value: string }
  | { type: "bearer"; value: string }

export interface CredentialRequest {
  providerId: ProviderId
  modelId: string
  endpointId: EndpointProfileId
  protocol: EndpointProtocol
}

export type CredentialResolver = (
  request: CredentialRequest,
) => ProviderCredential | undefined | Promise<ProviderCredential | undefined>

export interface CredentialOptions {
  apiKey?: string
  bearerToken?: string
  credentialResolver?: CredentialResolver
}

/** A deliberately non-sensitive credential failure. Never add resolver output to this error. */
export class CredentialResolutionError extends Error {
  readonly code: "credential_unavailable" | "credential_invalid" | "credential_resolver_failed"
  readonly providerId: ProviderId

  constructor(
    code: CredentialResolutionError["code"],
    providerId: ProviderId,
  ) {
    super(code === "credential_unavailable"
      ? `Missing credential for provider ${providerId}`
      : code === "credential_invalid"
        ? `Invalid credential for provider ${providerId}`
        : `Credential resolution failed for provider ${providerId}`)
    this.name = "CredentialResolutionError"
    this.code = code
    this.providerId = providerId
  }
}

/** Safe for diagnostics and event metadata. It intentionally has no credential value. */
export function redactCredential(credential: ProviderCredential): { type: ProviderCredential["type"] } {
  return { type: credential.type }
}

export async function resolveCredential(
  request: CredentialRequest,
  options: CredentialOptions,
): Promise<ProviderCredential> {
  const configured = configuredCredential(request.providerId, options)
  if (configured) return configured
  if (!options.credentialResolver) throw unavailable(request.providerId)

  let resolved: ProviderCredential | undefined
  try {
    resolved = await options.credentialResolver(request)
  } catch {
    throw new CredentialResolutionError("credential_resolver_failed", request.providerId)
  }
  return validateCredential(resolved, request.providerId)
}

/**
 * The legacy synchronous factory remains synchronous. A resolver which performs I/O must use
 * `resolveProviderRuntimeAsync` / `createProviderAsync` instead of risking an implicit request.
 */
export function resolveCredentialSync(
  request: CredentialRequest,
  options: CredentialOptions,
): ProviderCredential {
  const configured = configuredCredential(request.providerId, options)
  if (configured) return configured
  if (!options.credentialResolver) throw unavailable(request.providerId)

  let resolved: ProviderCredential | undefined | Promise<ProviderCredential | undefined>
  try {
    resolved = options.credentialResolver(request)
  } catch {
    throw new CredentialResolutionError("credential_resolver_failed", request.providerId)
  }
  if (isPromise(resolved)) {
    throw new CredentialResolutionError("credential_resolver_failed", request.providerId)
  }
  return validateCredential(resolved, request.providerId)
}

function configuredCredential(
  providerId: ProviderId,
  options: CredentialOptions,
): ProviderCredential | undefined {
  const sources = [
    options.apiKey === undefined ? undefined : { type: "api_key" as const, value: options.apiKey },
    options.bearerToken === undefined ? undefined : { type: "bearer" as const, value: options.bearerToken },
  ].filter((value): value is ProviderCredential => value !== undefined)
  if (sources.length > 1) throw new CredentialResolutionError("credential_invalid", providerId)
  return sources.length === 1 ? validateCredential(sources[0], providerId) : undefined
}

function validateCredential(
  credential: ProviderCredential | undefined,
  providerId: ProviderId,
): ProviderCredential {
  if (!credential) throw unavailable(providerId)
  if (
    (credential.type !== "api_key" && credential.type !== "bearer")
    || typeof credential.value !== "string"
    || credential.value.trim() === ""
  ) {
    throw new CredentialResolutionError("credential_invalid", providerId)
  }
  return credential
}

function unavailable(providerId: ProviderId): CredentialResolutionError {
  return new CredentialResolutionError("credential_unavailable", providerId)
}

function isPromise(value: unknown): value is Promise<unknown> {
  return Boolean(value) && typeof (value as { then?: unknown }).then === "function"
}
