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

export interface OAuthAccessToken {
  accessToken: string
  /** Epoch time on the clock supplied to the resolver. The token is invalid at this instant. */
  expiresAt: number
  scopes?: readonly string[]
  audience?: string
}

export interface OAuthRefreshRequest {
  providerId: ProviderId
  audience?: string
  requiredScopes: readonly string[]
}

export interface OAuthCredentialResolverOptions {
  providerId: ProviderId
  /** Provider-owned refresh/ADC bridge. The Host owns its transport and refresh token. */
  refresh: (request: OAuthRefreshRequest) => OAuthAccessToken | Promise<OAuthAccessToken>
  requiredScopes?: readonly string[]
  audience?: string
  clock?: () => number
}

export interface CredentialOptions {
  apiKey?: string
  bearerToken?: string
  credentialResolver?: CredentialResolver
}

/** A deliberately non-sensitive credential failure. Never add resolver output to this error. */
export class CredentialResolutionError extends Error {
  readonly code:
    | "credential_unavailable"
    | "credential_invalid"
    | "credential_resolver_failed"
    | "credential_refresh_failed"
    | "credential_oauth_scope_mismatch"
    | "credential_oauth_audience_mismatch"
    | "credential_revoked"
    | "credential_auth_mode_unsupported"
  readonly providerId: ProviderId
  readonly retryable: boolean

  constructor(
    code: CredentialResolutionError["code"],
    providerId: ProviderId,
    retryable = false,
  ) {
    super(messageForCredentialError(code, providerId))
    this.name = "CredentialResolutionError"
    this.code = code
    this.providerId = providerId
    this.retryable = retryable
  }
}

/**
 * Host-owned OAuth/ADC extension. It retains only a refreshable bearer in process memory,
 * never exposes refresh-token material, and coalesces concurrent refreshes for one provider.
 */
export class OAuthCredentialResolver {
  private readonly providerId: ProviderId
  private readonly refresh: OAuthCredentialResolverOptions["refresh"]
  private readonly requiredScopes: readonly string[]
  private readonly audience?: string
  private readonly clock: () => number
  private token?: OAuthAccessToken
  private refreshInFlight?: Promise<OAuthAccessToken>
  private revoked = false

  constructor(options: OAuthCredentialResolverOptions) {
    this.providerId = options.providerId
    this.refresh = options.refresh
    this.requiredScopes = [...(options.requiredScopes ?? [])]
    this.audience = options.audience
    this.clock = options.clock ?? Date.now
  }

  /** An arrow field intentionally remains bound when passed as `credentialResolver`. */
  readonly resolve: CredentialResolver = async request => {
    if (request.providerId !== this.providerId) throw unavailable(request.providerId)
    if (this.revoked) throw new CredentialResolutionError("credential_revoked", this.providerId)

    const token = this.usable(this.token) ? this.token : await this.refreshToken()
    this.assertUsable(token)
    if (this.revoked) throw new CredentialResolutionError("credential_revoked", this.providerId)
    return { type: "bearer", value: token.accessToken }
  }

  revoke(): void {
    this.revoked = true
    this.token = undefined
  }

  status(): { providerId: ProviderId; revoked: boolean; hasUsableToken: boolean } {
    return {
      providerId: this.providerId,
      revoked: this.revoked,
      hasUsableToken: !this.revoked && this.usable(this.token),
    }
  }

  private usable(token: OAuthAccessToken | undefined): token is OAuthAccessToken {
    return token !== undefined && Number.isFinite(token.expiresAt) && token.expiresAt > this.clock()
  }

  private async refreshToken(): Promise<OAuthAccessToken> {
    if (!this.refreshInFlight) {
      this.refreshInFlight = Promise.resolve()
        .then(() => this.refresh({
          providerId: this.providerId,
          ...(this.audience === undefined ? {} : { audience: this.audience }),
          requiredScopes: this.requiredScopes,
        }))
        .then(token => {
          this.assertUsable(token)
          if (this.revoked) throw new CredentialResolutionError("credential_revoked", this.providerId)
          this.token = token
          return token
        })
        .catch(error => {
          if (error instanceof CredentialResolutionError) throw error
          throw new CredentialResolutionError("credential_refresh_failed", this.providerId, true)
        })
        .finally(() => { this.refreshInFlight = undefined })
    }
    return this.refreshInFlight
  }

  private assertUsable(token: OAuthAccessToken): void {
    if (
      !token
      || typeof token.accessToken !== "string"
      || token.accessToken.trim() === ""
      || !Number.isFinite(token.expiresAt)
      || token.expiresAt <= this.clock()
    ) {
      throw new CredentialResolutionError("credential_invalid", this.providerId)
    }
    if (this.audience !== undefined && token.audience !== this.audience) {
      throw new CredentialResolutionError("credential_oauth_audience_mismatch", this.providerId)
    }
    const tokenScopes = new Set(token.scopes ?? [])
    if (this.requiredScopes.some(scope => !tokenScopes.has(scope))) {
      throw new CredentialResolutionError("credential_oauth_scope_mismatch", this.providerId)
    }
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
  } catch (error) {
    if (error instanceof CredentialResolutionError) throw error
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
  } catch (error) {
    if (error instanceof CredentialResolutionError) throw error
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

function messageForCredentialError(
  code: CredentialResolutionError["code"],
  providerId: ProviderId,
): string {
  switch (code) {
    case "credential_unavailable":
      return `Missing credential for provider ${providerId}`
    case "credential_invalid":
      return `Invalid credential for provider ${providerId}`
    case "credential_refresh_failed":
      return `Credential refresh failed for provider ${providerId}`
    case "credential_oauth_scope_mismatch":
      return `Credential scope does not satisfy provider ${providerId}`
    case "credential_oauth_audience_mismatch":
      return `Credential audience does not match provider ${providerId}`
    case "credential_revoked":
      return `Credential was revoked for provider ${providerId}`
    case "credential_auth_mode_unsupported":
      return `Credential authentication mode is unsupported for provider ${providerId}`
    case "credential_resolver_failed":
      return `Credential resolution failed for provider ${providerId}`
  }
}

function isPromise(value: unknown): value is Promise<unknown> {
  return Boolean(value) && typeof (value as { then?: unknown }).then === "function"
}
