import type { ModelRegistration } from "./model-registry.js"

export type { ModelRegistration } from "./model-registry.js"

/** A query-only model catalog. Catalogs describe models; selection policy belongs to CapabilityRouter. */
export interface ModelCatalog {
  list(): Promise<readonly ModelRegistration[]>
  get(modelId: string): Promise<ModelRegistration | undefined>
}

export interface ModelCatalogSource {
  list(): Promise<readonly ModelRegistration[]>
}

export type ModelCatalogRefreshResult =
  | { ok: true }
  | { ok: false; errorCode: "refresh_failed" }

/** Immutable, deterministic model facts supplied by the SDK or an application. */
export class StaticModelCatalog implements ModelCatalog {
  private readonly registrations: readonly ModelRegistration[]
  private readonly byId: ReadonlyMap<string, ModelRegistration>

  constructor(registrations: readonly ModelRegistration[]) {
    const byId = new Map<string, ModelRegistration>()
    for (const registration of registrations) {
      const id = registration.descriptor.id
      if (byId.has(id)) throw new Error(`Duplicate model catalog entry: ${id}`)
      byId.set(id, registration)
    }
    this.registrations = [...byId.values()].sort((a, b) => a.descriptor.id.localeCompare(b.descriptor.id))
    this.byId = byId
  }

  async list(): Promise<readonly ModelRegistration[]> {
    return this.registrations
  }

  async get(modelId: string): Promise<ModelRegistration | undefined> {
    return this.byId.get(modelId)
  }
}

/**
 * A host-owned discovery cache. Refresh never throws and never deletes the last good snapshot;
 * callers can keep using the static catalog when a remote provider is unavailable.
 */
export class DynamicModelCatalog implements ModelCatalog {
  private snapshot = new Map<string, ModelRegistration>()

  constructor(
    private readonly source: ModelCatalogSource,
    private readonly fallback: ModelCatalog = new StaticModelCatalog([]),
  ) {}

  async list(): Promise<readonly ModelRegistration[]> {
    const merged = new Map<string, ModelRegistration>()
    for (const registration of await this.fallback.list()) merged.set(registration.descriptor.id, registration)
    for (const registration of this.snapshot.values()) merged.set(registration.descriptor.id, registration)
    return [...merged.values()].sort((a, b) => a.descriptor.id.localeCompare(b.descriptor.id))
  }

  async get(modelId: string): Promise<ModelRegistration | undefined> {
    return this.snapshot.get(modelId) ?? await this.fallback.get(modelId)
  }

  async refresh(): Promise<ModelCatalogRefreshResult> {
    try {
      const next = new Map<string, ModelRegistration>()
      for (const registration of await this.source.list()) {
        const id = registration.descriptor.id
        if (next.has(id)) throw new Error("duplicate dynamic model catalog entry")
        next.set(id, registration)
      }
      this.snapshot = next
      return { ok: true }
    } catch {
      return { ok: false, errorCode: "refresh_failed" }
    }
  }
}
